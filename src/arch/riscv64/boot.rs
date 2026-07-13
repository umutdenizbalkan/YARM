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

    // Boot-hart selection: per the SBI spec, OpenSBI's generic firmware
    // (used by QEMU virt) releases exactly ONE hart to the kernel entry
    // point (Domain0 Next Address). Every other hart never reaches this
    // code at all -- it stays parked *inside OpenSBI itself*
    // (sbi_hsm_hart_wait), awaiting an explicit HSM hart_start call. So
    // whichever hart executes `_start` IS the boot hart, unconditionally
    // and regardless of its hart-id; there is no cold-boot race to
    // resolve here. (A prior "first arrival wins" atomic-swap-based CAS
    // guard solved a race that cannot occur under this guarantee, and
    // additionally required the `Zaamo` extension, which is not enabled
    // for this target -- it silently failed to assemble for every real
    // riscv64gc build.) Record the OpenSBI hart-id unconditionally so the
    // Rust side can read it later (park-secondaries, topology marker).
    la t2, RISCV64_BOOT_HART_ID
    sd s0, (t2)

    // Hand the firmware registers to the Rust primary entry, which emits
    // the early boot markers and then calls the common kernel entry.
    mv a0, s0                       // a0 = hartid
    mv a1, s1                       // a1 = DTB pointer
    call yarm_riscv64_primary_entry // -> ! (does not return)
1:
    wfi
    j 1b

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

/// Boot-hart id captured from OpenSBI's `a0` on the boot hart's first
/// entry into `_start`. Lives in `.bss`; the asm path writes this once
/// before calling `yarm_riscv64_primary_entry`.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
#[unsafe(no_mangle)]
static mut RISCV64_BOOT_HART_ID: u64 = 0;

/// Returns the captured OpenSBI boot-hart id. Valid after the asm prologue
/// of `_start` has run; before that returns 0.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub fn boot_hart_id() -> usize {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(RISCV64_BOOT_HART_ID)) as usize }
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "riscv64")))]
pub fn boot_hart_id() -> usize {
    0
}

/// Captured DTB slice for late consumers (PLIC discovery, future
/// interrupt-source enumeration). Set once from `prepare_arch_boot` after
/// the FDT magic + size checks pass. Lives in `.bss` so a missing
/// `save_dtb_for_late_consumers` call simply yields `None`.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static CAPTURED_DTB_PTR: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static CAPTURED_DTB_LEN: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn save_dtb_for_late_consumers(dtb: &'static [u8]) {
    CAPTURED_DTB_PTR.store(dtb.as_ptr() as usize, core::sync::atomic::Ordering::Release);
    CAPTURED_DTB_LEN.store(dtb.len(), core::sync::atomic::Ordering::Release);
}

/// Returns the captured DTB slice, if one was saved during
/// `prepare_arch_boot`. Late consumers (e.g. PLIC discovery from the
/// idle-safe point) read this to walk the FDT a second time without
/// re-resolving the OpenSBI handoff pointer.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub fn captured_dtb() -> Option<&'static [u8]> {
    let ptr = CAPTURED_DTB_PTR.load(core::sync::atomic::Ordering::Acquire);
    let len = CAPTURED_DTB_LEN.load(core::sync::atomic::Ordering::Acquire);
    if ptr == 0 || len == 0 {
        return None;
    }
    Some(unsafe { core::slice::from_raw_parts(ptr as *const u8, len) })
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "riscv64")))]
pub fn captured_dtb() -> Option<&'static [u8]> {
    None
}

/// UART line-lock to serialize multi-hart `console_putchar` SBI output.
///
/// `early_sbi_marker` writes one byte at a time via legacy SBI; concurrent
/// emitters from boot hart + HSM-started secondaries interleave bytes and
/// produce garbled markers. The lock is taken per-line and released after
/// the trailing CRLF, so a hart that wins the lock writes a complete line
/// atomically (from the reader's perspective). Lives in `.bss`, so it is
/// safe to use before any allocator/bootstrap step.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static EARLY_MARKER_LOCK: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn early_marker_lock_acquire() {
    while EARLY_MARKER_LOCK
        .compare_exchange(
            false,
            true,
            core::sync::atomic::Ordering::Acquire,
            core::sync::atomic::Ordering::Relaxed,
        )
        .is_err()
    {
        core::hint::spin_loop();
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn early_marker_lock_release() {
    EARLY_MARKER_LOCK.store(false, core::sync::atomic::Ordering::Release);
}

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
    // Take the per-line UART lock so concurrent emitters from the boot hart
    // and HSM-started secondaries cannot interleave bytes mid-line.
    early_marker_lock_acquire();
    crate::arch::riscv64::console::write_line(text);
    early_marker_lock_release();
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
    // Confirm the asm-captured OpenSBI hart-id matches what's now in `a0`.
    // The slot was written unconditionally from `_start`; this breadcrumb
    // proves the captured value matches the live one so any future asm
    // refactor cannot silently drift the boot-hart id.
    early_marker!("RISCV_BOOT_HART_ID_STORED hart={}", boot_hart_id());
    // Park every non-bootstrap hart in a safe Rust loop BEFORE the boot hart
    // touches BSS, the allocator, cmdline capture, or kernel bootstrap.
    park_secondary_harts_early();
    early_marker!("RISCV_DTB_PTR value=0x{:x}", dtb_ptr);
    unsafe { yarm_kernel_main(dtb_ptr) }
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
// `yarm_riscv64_trap_vector` is the S-mode trap entry. On entry sp is the user
// sp; we swap it with the kernel trap stack stored in sscratch, then push a
// full `RiscvTrapFrame` (all 31 GPRs except x0, plus user_sp/sepc/sstatus/
// scause/stval). The frame pointer is passed to the Rust bridge which builds
// the generic TrapFrame, dispatches via the existing handle_trap_entry path,
// then writes return values into the frame. The asm tail
// `yarm_riscv64_trap_return` restores all GPRs from the frame, rewrites
// sscratch with the (possibly updated) user sp, and `sret`s.
//
// Frame offsets (in bytes) - 38 u64 slots = 304 bytes, 16-byte aligned:
//   0..=7   user x1 (ra)
//   8..=15  user x2 (sp at trap entry, BEFORE sscratch swap)
//   16..=23 user x3 (gp)
//   ...
//   240..   user x31
//   248..   sepc
//   256..   sstatus
//   264..   scause
//   272..   stval
//   280..   reserved
//   288..   reserved
//   296..   reserved
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
    // S-mode for trap bookkeeping if/when we resume there.
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
    // Reserve 304 bytes (38 * 8) for the RiscvTrapFrame, 16-byte aligned.
    addi sp, sp, -304
    // Save GPRs x1, x3..x31 at their natural offsets (x0 omitted).
    sd x1,   0(sp)             // ra
    // x2 (user sp) is in sscratch right now; we'll write it later.
    sd x3,  16(sp)             // gp
    sd x4,  24(sp)             // tp
    sd x5,  32(sp)             // t0
    sd x6,  40(sp)             // t1
    sd x7,  48(sp)             // t2
    sd x8,  56(sp)             // s0/fp
    sd x9,  64(sp)             // s1
    sd x10, 72(sp)             // a0
    sd x11, 80(sp)             // a1
    sd x12, 88(sp)             // a2
    sd x13, 96(sp)             // a3
    sd x14, 104(sp)            // a4
    sd x15, 112(sp)            // a5
    sd x16, 120(sp)            // a6
    sd x17, 128(sp)            // a7  (syscall number)
    sd x18, 136(sp)            // s2
    sd x19, 144(sp)            // s3
    sd x20, 152(sp)            // s4
    sd x21, 160(sp)            // s5
    sd x22, 168(sp)            // s6
    sd x23, 176(sp)            // s7
    sd x24, 184(sp)            // s8
    sd x25, 192(sp)            // s9
    sd x26, 200(sp)            // s10
    sd x27, 208(sp)            // s11
    sd x28, 216(sp)            // t3
    sd x29, 224(sp)            // t4
    sd x30, 232(sp)            // t5
    sd x31, 240(sp)            // t6
    // Save user x2 (sp) — currently in sscratch.
    csrr t0, sscratch
    sd t0, 8(sp)
    // Capture CSRs into the frame.
    csrr t0, sepc
    sd t0, 248(sp)
    csrr t0, sstatus
    sd t0, 256(sp)
    csrr t0, scause
    sd t0, 264(sp)
    csrr t0, stval
    sd t0, 272(sp)
    // Re-point sscratch at the kernel trap stack top so a nested trap (if any)
    // does not corrupt this frame. The top is the *original* trap stack top,
    // which equals (sp + 304) at this point.
    addi t0, sp, 304
    csrw sscratch, t0
    // Call the Rust bridge with a0 = pointer to the saved frame.
    mv a0, sp
    call yarm_riscv64_trap_bridge
    // Fall through to the restore/sret tail.

    .global yarm_riscv64_trap_return
    .type yarm_riscv64_trap_return, @function
yarm_riscv64_trap_return:
    // a0 = *const RiscvTrapFrame (== sp on the fall-through path).
    // Restore CSRs first.
    ld t0, 248(a0)
    csrw sepc, t0
    ld t0, 256(a0)
    csrw sstatus, t0
    // Save user sp into sscratch so the next trap can swap it in.
    ld t0, 8(a0)
    csrw sscratch, t0
    // Restore GPRs from the frame.
    ld x1,   0(a0)
    ld x3,  16(a0)
    ld x4,  24(a0)
    ld x5,  32(a0)
    ld x6,  40(a0)
    ld x7,  48(a0)
    ld x8,  56(a0)
    ld x9,  64(a0)
    // x10 (a0) restored last so we can keep using the pointer.
    ld x11, 80(a0)
    ld x12, 88(a0)
    ld x13, 96(a0)
    ld x14, 104(a0)
    ld x15, 112(a0)
    ld x16, 120(a0)
    ld x17, 128(a0)
    ld x18, 136(a0)
    ld x19, 144(a0)
    ld x20, 152(a0)
    ld x21, 160(a0)
    ld x22, 168(a0)
    ld x23, 176(a0)
    ld x24, 184(a0)
    ld x25, 192(a0)
    ld x26, 200(a0)
    ld x27, 208(a0)
    ld x28, 216(a0)
    ld x29, 224(a0)
    ld x30, 232(a0)
    ld x31, 240(a0)
    // Swap sp <-> sscratch: sp gets the user sp, sscratch gets the kernel trap
    // stack top (saved during the entry tail).
    csrrw sp, sscratch, sp
    // Finally restore a0.
    ld a0, 72(a0)
    sret
"#
);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
#[repr(C, align(16))]
pub(crate) struct RiscvTrapFrame {
    pub(crate) regs: [u64; 31],     // x1..x31 (x0 omitted) at offsets 0..=240
    pub(crate) sepc: u64,           // 248
    pub(crate) sstatus: u64,        // 256
    pub(crate) scause: u64,         // 264
    pub(crate) stval: u64,          // 272
    pub(crate) _reserved: [u64; 3], // 280, 288, 296
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
impl RiscvTrapFrame {
    // Register-index helpers (xN -> regs[N-1]).
    pub(crate) const RA: usize = 0; // x1
    pub(crate) const SP: usize = 1; // x2
    pub(crate) const GP: usize = 2; // x3
    pub(crate) const TP: usize = 3; // x4
    pub(crate) const A0: usize = 9; // x10
    pub(crate) const A1: usize = 10; // x11
    pub(crate) const A2: usize = 11; // x12
    pub(crate) const A3: usize = 12; // x13
    pub(crate) const A4: usize = 13; // x14
    pub(crate) const A5: usize = 14; // x15
    pub(crate) const A7: usize = 16; // x17
    pub(crate) const S2: usize = 17; // x18 (callee-saved; banks fork return child_tid)
}

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
    fn yarm_riscv64_trap_return(frame: *const RiscvTrapFrame) -> !;
    static yarm_riscv64_trap_vector: u8;
}

/// Trap-time `SharedKernel` pointer, installed by `run_with_prepared_kernel`
/// after `Bootstrap::init_shared_static` has constructed the boot-owned
/// `SharedKernel` in `.bss` (BOOTSTRAP_SHARED_KERNEL). The Rust trap bridge
/// loads this to dispatch through the Stage 196A RISC-V shared wrapper
/// (`handle_riscv_trap_entry_shared`), which owns the bounded `with_cpu`
/// broad-lock phase and the post-lock drain — the direct-call raw
/// `&'static mut KernelState` path is retired. Null until installed; once set
/// the pointer outlives all traps because BOOTSTRAP_SHARED_KERNEL lives in
/// .bss (inside the kernel-shared Sv39 gigapage) for the life of the kernel.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static RISCV_TRAP_SHARED_KERNEL_PTR: core::sync::atomic::AtomicPtr<crate::runtime::SharedKernel> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub(crate) fn install_riscv_trap_shared_kernel(shared: &'static crate::runtime::SharedKernel) {
    RISCV_TRAP_SHARED_KERNEL_PTR.store(
        shared as *const _ as *mut _,
        core::sync::atomic::Ordering::SeqCst,
    );
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn trap_shared_kernel_riscv() -> Option<&'static crate::runtime::SharedKernel> {
    let ptr = RISCV_TRAP_SHARED_KERNEL_PTR.load(core::sync::atomic::Ordering::SeqCst);
    if ptr.is_null() {
        None
    } else {
        // SAFETY: the pointer was installed from a `&'static SharedKernel`
        // (BOOTSTRAP_SHARED_KERNEL, .bss) and is never mutated after install,
        // so re-forming a shared reference is sound.
        Some(unsafe { &*(ptr as *const crate::runtime::SharedKernel) })
    }
}

/// Set once we have observed at least one successful round-trip
/// (handle -> restore -> sret -> RISCV_USER_RESUMED). After that the
/// RISCV_LIVEEEEEEE marker is emitted and further round-trips drop to a
/// concise "RISCV_SYSCALL_ROUNDTRIP_OK" so the log stays readable.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static RISCV_FIRST_ROUNDTRIP_LOGGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

// Stage 196A: the S-mode trap runs the ENTIRE syscall dispatch on this dedicated
// trap stack (the trap vector swaps sp←sscratch = `riscv_trap_stack_top`). The
// deepest RISC-V dispatch chain (IPC cap-transfer / SpawnV5 / fork, with large
// on-stack `no_std` temporaries) uses well over 256 KiB — the former **16 KiB**
// stack was a PRE-EXISTING latent overflow: deep traps had been silently
// clobbering whatever `.bss` sat below the stack, tolerated only because the
// corrupted bytes happened to land on benign/padding statics. Stage 196A's new
// default-off oracle flag landed in that blast radius and made the overflow
// visible as a non-deterministic false→true flip (which vanished at ≥1 MiB but
// not at 256 KiB — bounding the deepest dispatch between 256 KiB and 1 MiB).
// Size the trap stack at 2 MiB for solid headroom over the observed worst case.
// It lives in `.bss` (NOLOAD, inside the kernel-shared gigapage), so it costs
// RAM only — not image size — and the 16 MiB boot stack dwarfs it.
//
// TODO(riscv-trap-stack-debt): 2 MiB is an **emergency correctness size**, not a
// measured bound. Before RISC-V SMP scaling (a per-hart trap stack of this size
// multiplies RAM cost), future work MUST: (1) measure the true maximum trap-time
// stack depth (e.g. a stack-canary/watermark pass over the deepest dispatch
// chains); (2) shrink the large on-stack `no_std` temporaries in the deepest
// paths (IPC cap-transfer / SpawnV5 / fork) so the trap stack can be reduced;
// (3) only then lower this size to the measured worst case + margin. Do NOT
// reduce it before that measurement — the 16 KiB overflow was silent.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
const RISCV_TRAP_STACK_SIZE: usize = 2 * 1024 * 1024;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
#[repr(align(16))]
struct RiscvTrapStack([u8; RISCV_TRAP_STACK_SIZE]);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
static mut RISCV_TRAP_STACK: RiscvTrapStack = RiscvTrapStack([0; RISCV_TRAP_STACK_SIZE]);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn riscv_trap_stack_top() -> u64 {
    let base = core::ptr::addr_of!(RISCV_TRAP_STACK) as u64;
    (base + (RISCV_TRAP_STACK_SIZE as u64)) & !0xf
}

/// S-mode trap bridge. Builds a generic `TrapFrame` from the saved RISC-V
/// register frame, dispatches through the existing
/// `crate::arch::riscv64::trap::handle_trap_entry` Rust path (which decodes
/// the trap event, advances sepc by +4 on user ecalls, runs the syscall
/// dispatcher, and reapplies the resumed thread's `UserRegisterContext`),
/// writes the syscall return values back into the saved frame, then tail-calls
/// the asm restore/sret tail. The bridge is `-> !` because the asm tail does
/// not return.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_riscv64_trap_bridge(frame_ptr: *mut RiscvTrapFrame) -> ! {
    const EXC_USER_ECALL: usize = 8;
    let frame = unsafe { &mut *frame_ptr };
    let scause = frame.scause as usize;
    let sepc = frame.sepc as usize;
    let stval = frame.stval as usize;
    let sstatus = frame.sstatus as usize;
    let user_sp = frame.regs[RiscvTrapFrame::SP] as usize;
    let from_u = (sstatus >> 8) & 1 == 0;

    let first_trap = !RISCV_FIRST_ROUNDTRIP_LOGGED.load(core::sync::atomic::Ordering::Acquire);
    if first_trap {
        early_marker!(
            "RISCV_TRAP_ENTER scause=0x{:x} sepc=0x{:x} stval=0x{:x} sstatus=0x{:x} spp={} from_u={} user_sp=0x{:x}",
            scause,
            sepc,
            stval,
            sstatus,
            (sstatus >> 8) & 1,
            from_u as u8,
            user_sp
        );
        early_marker!(
            "RISCV_FIRST_USER_TRAP scause=0x{:x} sepc=0x{:x} stval=0x{:x}",
            scause,
            sepc,
            stval
        );
    }

    // Stage 196A: the bridge no longer holds a persistent raw `&'static mut
    // KernelState`. It borrows the boot-owned `SharedKernel` and performs every
    // kernel interaction through a bounded `with_cpu` (or the shared wrapper),
    // so no broad `&mut KernelState` escapes a bounded callback.
    let Some(shared) = trap_shared_kernel_riscv() else {
        early_marker!("RISCV_TRAP_HANDLE_FAILED reason=no_trap_shared_kernel");
        riscv_trap_halt("no_trap_shared_kernel");
    };
    // RISC-V is BSP-only (`online_cpus==1`); the trapping CPU is always the
    // bootstrap hart. `with_cpu` rebinds `current_cpu` to this value anyway.
    let cpu = crate::kernel::scheduler::CpuId(crate::arch::platform_constants::BOOTSTRAP_CPU_ID);
    let entering_tid = shared.current_tid_authoritative(cpu).unwrap_or(0);

    // ── Phase: SAVE_DONE ────────────────────────────────────────────────
    if first_trap {
        early_marker!(
            "RISCV_TRAP_SAVE_BEGIN tid={} scause=0x{:x} sepc=0x{:x} stval=0x{:x}",
            entering_tid,
            scause,
            sepc,
            stval
        );
        early_marker!(
            "RISCV_TRAP_SAVE_DONE tid={} scause=0x{:x} sepc=0x{:x} stval=0x{:x}",
            entering_tid,
            scause,
            sepc,
            stval
        );
    }

    if !from_u {
        // The trap was taken from S-mode — kernel fault. We have no fallback
        // path; report and halt with the named step so the user sees the
        // exact failure point rather than a silent loop.
        early_marker!(
            "RISCV_TRAP_UNHANDLED scause=0x{:x} sepc=0x{:x} stval=0x{:x} sstatus=0x{:x} reason=trap_from_s_mode",
            scause,
            sepc,
            stval,
            sstatus
        );
        riscv_trap_halt("trap_from_s_mode");
    }

    // Build the generic TrapFrame from the saved register file.
    let mut tframe = crate::kernel::trapframe::TrapFrame::zeroed();
    // For ecall we pre-advance saved_pc by 4 so the TCB snapshot taken inside
    // `sync_current_thread_from_frame` captures sepc+4 as the post-ecall PC.
    // This is the sole PC-advance for RISC-V ecalls: handle_trap_entry (Stage
    // 163L+) does NOT add its own +4 because restore_arch_thread_state reloads
    // saved_pc from the TCB (sepc+4), and a redundant +4 would double-advance
    // to sepc+8 causing an instruction page fault (Stage 163M regression fix).
    // Without this pre-advance the blocked task's TCB would hold the raw ecall
    // address, causing an infinite ecall loop on resume.
    let advance = if scause == EXC_USER_ECALL { 4 } else { 0 };
    tframe.set_saved_pc(sepc + advance);
    tframe.set_saved_sp(user_sp);
    // user_gprs[i] mirrors xN where i = N (slot 0 = x0 = 0).
    tframe.set_user_gpr(0, 0); // x0
    for n in 1..32usize {
        tframe.set_user_gpr(n, frame.regs[n - 1] as usize);
    }
    // RISC-V Linux-style syscall ABI: a7 = syscall number, a0..a5 = args.
    if scause == EXC_USER_ECALL {
        tframe.set_syscall_num(frame.regs[RiscvTrapFrame::A7] as usize);
        tframe.set_arg(0, frame.regs[RiscvTrapFrame::A0] as usize);
        tframe.set_arg(1, frame.regs[RiscvTrapFrame::A1] as usize);
        tframe.set_arg(2, frame.regs[RiscvTrapFrame::A2] as usize);
        tframe.set_arg(3, frame.regs[RiscvTrapFrame::A3] as usize);
        tframe.set_arg(4, frame.regs[RiscvTrapFrame::A4] as usize);
        tframe.set_arg(5, frame.regs[RiscvTrapFrame::A5] as usize);
        if first_trap {
            early_marker!("RISCV_FIRST_USER_SYSCALL nr={}", tframe.syscall_num());
            early_marker!(
                "RISCV_SYSCALL_DECODE nr={} a0=0x{:x} a1=0x{:x} a2=0x{:x} a3=0x{:x} a4=0x{:x} a5=0x{:x}",
                tframe.syscall_num(),
                tframe.arg(0),
                tframe.arg(1),
                tframe.arg(2),
                tframe.arg(3),
                tframe.arg(4),
                tframe.arg(5)
            );
            early_marker!(
                "RISCV_TRAP_HANDLE_BEGIN tid={} nr={}",
                entering_tid,
                tframe.syscall_num()
            );
        }
    }

    // For ecall the pre-advanced PC (sepc+4) is snapshotted into the TCB by
    // `sync_current_thread_from_frame`. Stage 163L's restore_arch_thread_state
    // reloads that sepc+4 into tframe.saved_pc; handle_trap_entry does NOT apply
    // its own +4 (Stage 163M fix), so the net resumed PC is sepc+4 exactly once.
    let ctx = crate::arch::riscv64::trap::Riscv64TrapContext { scause, stval };
    // Stage 196A: route through the RISC-V shared trap-entry wrapper. It owns the
    // `GLOBAL_LOCK_DROP_TRAP_PATH_ACTIVE` flag lifecycle, runs the UNCHANGED
    // canonical handler inside a bounded `with_cpu` broad-lock phase, and drains
    // post-lock work after the guard drops. The split dispatcher declines every
    // RISC-V syscall (zero retirement classes enabled in this foundation stage).
    let handle_result =
        crate::arch::riscv64::trap::handle_riscv_trap_entry_shared(shared, cpu, ctx, &mut tframe);

    if let Err(err) = handle_result {
        // The generic restore_arch_thread_state returns Err(Internal) when
        // there is no runnable user task to resume (all services blocked on
        // IPC recv, init parked). For an event-driven microkernel awaiting
        // I/O this is the correct terminal state — wfi here instead of
        // treating it as a fatal trap.
        let next_tid = shared.current_tid_authoritative(cpu).unwrap_or(0);
        if next_tid == 0 {
            crate::yarm_log!(
                "RISCV_KERNEL_IDLE_WAITING_FOR_IO reason=no_runnable_task all_services_blocked"
            );
            // Safe point per the timer/PLIC bring-up contract: real S-mode
            // trap vector + kernel-state pointer are installed; service
            // chain has reached stable idle. Both init paths default to
            // deferred and never enable STIE / external-IRQ delivery
            // until explicitly audited.
            let _ = crate::arch::riscv64::timer::init_timer_after_idle_safe_point();
            let _ = crate::arch::riscv64::plic::init_plic_after_idle_safe_point();
            riscv_trap_halt("kernel_idle_awaiting_io");
        }
        early_marker!(
            "RISCV_TRAP_HANDLE_FAILED reason=handle_trap_entry_err err={:?}",
            err
        );
        riscv_trap_halt("handle_trap_entry_err");
    }

    if first_trap && scause == EXC_USER_ECALL {
        early_marker!(
            "RISCV_TRAP_HANDLE_DONE status=ok ret0=0x{:x} ret1=0x{:x} ret2=0x{:x} err=0x{:x}",
            tframe.ret0(),
            tframe.ret1(),
            tframe.ret2(),
            tframe.error_code().unwrap_or(0)
        );
    }

    // Write back: handle_trap_entry has already applied the resumed thread's
    // UserRegisterContext to the frame (PC/SP/args/user_gprs). Resolve task
    // switch first so we know whether to source registers from the syscall
    // return path (same task) or from the resumed task's saved
    // `UserRegisterContext.args` (different task; first-run on fresh spawn or
    // resume after IPC block).
    let resume_tid = shared
        .current_tid_authoritative(cpu)
        .unwrap_or(entering_tid);
    let task_switched = resume_tid != entering_tid;
    if crate::kernel::boot::ipc_recv_proof_sender_wake_active() {
        crate::yarm_log!(
            "RISCV_FORK_PARENT_A0_EXPORT entering_tid={} resume_tid={} task_switched={} scause={:#x}",
            entering_tid,
            resume_tid,
            task_switched,
            scause
        );
    }

    if task_switched || scause == EXC_USER_ECALL {
        // PC/SP come from the (possibly task-switched) generic frame.
        frame.sepc = tframe.saved_pc() as u64;
        frame.regs[RiscvTrapFrame::SP] = tframe.saved_sp() as u64;

        // Mirror non-SP, non-ABI integer GPRs from the generic frame so non-ABI
        // registers are restored from the resumed task's saved context. SP comes
        // from `saved_sp` (above) and the A0..A5/A7 lanes are written below
        // depending on switch/syscall semantics — including them in this mirror
        // would clobber the canonical SP with the fresh-spawn user_gprs[2]==0 and
        // overwrite the ABI args/returns with stale user_gpr values.
        for n in 1..32usize {
            let i = n - 1;
            if i == RiscvTrapFrame::SP
                || i == RiscvTrapFrame::A0
                || i == RiscvTrapFrame::A1
                || i == RiscvTrapFrame::A2
                || i == RiscvTrapFrame::A3
                || i == RiscvTrapFrame::A4
                || i == RiscvTrapFrame::A5
                || i == RiscvTrapFrame::A7
            {
                continue;
            }
            frame.regs[i] = tframe.user_gpr(n) as usize as u64;
        }

        if task_switched {
            // First instruction the resumed task will execute consumes the YARM
            // startup ABI in a0..a5. UserRegisterContext stores those in
            // `args[0..5]` (apply_user_context copied them into `tframe.args`).
            // The freshly-spawned task's `user_gprs` are still all-zero, so
            // mirroring those would clobber the startup args — write args[]
            // directly into a0..a5. Correctness of these stores no longer relies
            // on the RISCV_STARTUP_ARGS log below (which is now pure
            // observability): the trap-return tail reads the frame back through a
            // pointer DERIVED FROM this same `&mut frame` (see the resume site), so
            // LLVM models the extern read as observing these stores and cannot
            // dead-strip them.
            //
            // RISCV_STARTUP_ARGS is a required startup-cap attestation marker for
            // each task resumed via this write-back path. It is NO LONGER
            // load-bearing for correctness (the provenance fix at the resume site
            // is) — it is retained purely for boot observability.
            crate::yarm_log!(
                "RISCV_STARTUP_ARGS tid={} entering={} a0={} a1={} a2={} a3={} a4={} a5={}",
                resume_tid,
                entering_tid,
                tframe.arg(0),
                tframe.arg(1),
                tframe.arg(2),
                tframe.arg(3),
                tframe.arg(4),
                tframe.arg(5)
            );
            frame.regs[RiscvTrapFrame::A0] = tframe.arg(0) as u64;
            frame.regs[RiscvTrapFrame::A1] = tframe.arg(1) as u64;
            frame.regs[RiscvTrapFrame::A2] = tframe.arg(2) as u64;
            frame.regs[RiscvTrapFrame::A3] = tframe.arg(3) as u64;
            frame.regs[RiscvTrapFrame::A4] = tframe.arg(4) as u64;
            frame.regs[RiscvTrapFrame::A5] = tframe.arg(5) as u64;
            // a7 holds the syscall-number lane on RISC-V; on first run it can
            // remain whatever the user code expects (0 for a freshly-zeroed
            // context is fine — task hasn't issued any ecall yet).
            frame.regs[RiscvTrapFrame::A7] = 0;
        } else {
            // Same task continuing past its own ecall: YARM ABI returns
            // a0=ret0, a1=ret1, a2=ret2, a3=error (mirrors AArch64).
            if let Some(err) = tframe.error_code() {
                frame.regs[RiscvTrapFrame::A0] = err as u64;
                frame.regs[RiscvTrapFrame::A1] = 0;
                frame.regs[RiscvTrapFrame::A2] = 0;
                frame.regs[RiscvTrapFrame::A3] = err as u64;
            } else {
                if crate::kernel::boot::ipc_recv_proof_sender_wake_active() {
                    crate::yarm_log!(
                        "RISCV_TCB_A0_SAVE_AFTER_EXPORT tid={} ret0={} ret1={} err={}",
                        resume_tid,
                        tframe.ret0(),
                        tframe.ret1(),
                        tframe.error
                    );
                }
                frame.regs[RiscvTrapFrame::A0] = tframe.ret0() as u64;
                frame.regs[RiscvTrapFrame::A1] = tframe.ret1() as u64;
                frame.regs[RiscvTrapFrame::A2] = tframe.ret2() as u64;
                frame.regs[RiscvTrapFrame::A3] = 0;
            }
            // a4, a5, a7 keep their user-side values from the saved frame at
            // trap time (they're caller-saved temps in the RISC-V ABI but YARM
            // userspace may rely on a7 staying as the syscall nr until next call).
            // The asm save preserved them and the mirror loop skipped them.
            frame.regs[RiscvTrapFrame::A4] = tframe.user_gpr(14) as u64;
            frame.regs[RiscvTrapFrame::A5] = tframe.user_gpr(15) as u64;
            frame.regs[RiscvTrapFrame::A7] = tframe.user_gpr(17) as u64;
        }
    } else {
        // Stage 163P: same-task NON-SYSCALL trap (e.g. COW/demand page fault).
        //
        // The hardware-saved `frame` (regs[], sepc, SP — captured by the ASM
        // trap entry exactly as the CPU trapped) IS the precise live state to
        // resume. We deliberately leave the ENTIRE frame untouched here.
        //
        // Crucially we must NOT mirror the generic `tframe` over it. Inside
        // handle_trap_entry, `restore_arch_thread_state` → `apply_user_context`
        // reloaded `tframe.user_gprs`/`args` from the TCB's user_context, which
        // was last synced at the previous *syscall* entry
        // (`sync_current_thread_from_frame`, only on the Syscall arm — page
        // faults never re-sync). That snapshot predates every register write
        // userspace made AFTER returning from that syscall. The fork return is
        // the fatal case: the parent's `ecall` returns child_tid in a0, the
        // very next userspace instruction does `mv s2, a0` to bank child_tid in
        // a callee-saved register, and the FIRST stack store after that is the
        // COW write that faults here (scause=0xf). Mirroring the stale snapshot
        // would reset s2 (and t0-t6/s0-s11/ra) to their pre-fork values —
        // overwriting child_tid in s2 with whatever s2 held before the fork
        // (observed: a stale format-string text address ~0x4073d9), which
        // userspace then stores as the fork return. Preserving the hardware
        // frame re-executes the faulting instruction with the exact registers
        // the CPU trapped on, so the COW copy is transparent to userspace.
        //
        // This is a general RISC-V correctness fix (any callee-saved/temp reg
        // mutated since the last syscall must survive a fault), not a proof
        // special-case.
        if crate::kernel::boot::ipc_recv_proof_sender_wake_active() {
            crate::yarm_log!(
                "RISCV_NON_SYSCALL_TRAP_FRAME_SAVE tid={} scause={:#x} sepc={:#x}",
                resume_tid,
                scause,
                frame.sepc
            );
            crate::yarm_log!(
                "RISCV_PAGE_FAULT_PRESERVE_GPRS tid={} a0={:#x} a1={:#x} s2={:#x} sp={:#x}",
                resume_tid,
                frame.regs[RiscvTrapFrame::A0],
                frame.regs[RiscvTrapFrame::A1],
                frame.regs[RiscvTrapFrame::S2],
                frame.regs[RiscvTrapFrame::SP]
            );
            crate::yarm_log!(
                "RISCV_POST_FAULT_TRAP_RETURN tid={} a0={:#x} s2={:#x}",
                resume_tid,
                frame.regs[RiscvTrapFrame::A0],
                frame.regs[RiscvTrapFrame::S2]
            );
            if frame.regs[RiscvTrapFrame::A0] != 0 {
                crate::yarm_log!(
                    "RISCV_FORK_PARENT_A0_PRESERVED_AFTER_FAULT tid={} a0={:#x}",
                    resume_tid,
                    frame.regs[RiscvTrapFrame::A0]
                );
            }
        }
    }

    // The active satp may have changed if dispatch_next_task picked a
    // different task — switch_address_space currently defers on RISC-V
    // (see hal_adapters.rs) so the satp installed at enter-user is still
    // live. Activate the new task's satp here explicitly so the sret lands
    // in the right user page table. The asid lookup is a bounded read through
    // the shared kernel (Stage 196A: no persistent raw `&mut KernelState`).
    let resume_asid = shared
        .with_cpu(cpu, |k| k.task_asid(resume_tid))
        .ok()
        .flatten();
    if let Some(asid) = resume_asid {
        // Make sure the kernel-shared gigapage is present in the resumed
        // task's page table. Idempotent for asids that already have it.
        let _ = crate::arch::riscv64::page_table::map_kernel_shared_into_asid(asid);
        if let Some(satp) = crate::arch::riscv64::page_table::cr3_for_asid(asid) {
            crate::arch::riscv64::page_table::write_satp(satp);
        }
    }

    let pc_final = frame.sepc;
    let sp_final = frame.regs[RiscvTrapFrame::SP];
    if first_trap {
        early_marker!("RISCV_TRAP_RESTORE_BEGIN tid={}", resume_tid);
        early_marker!(
            "RISCV_TRAP_RETURN_SRET tid={} pc=0x{:x} sp=0x{:x}",
            resume_tid,
            pc_final,
            sp_final
        );
        if RISCV_FIRST_ROUNDTRIP_LOGGED
            .compare_exchange(
                false,
                true,
                core::sync::atomic::Ordering::AcqRel,
                core::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
        {
            // Per the task brief: emit this banner when the round-trip is
            // about to perform a real sret back to U-mode.
            crate::arch::riscv64::console::write_line("RISCV_LIVEEEEEEE");
            early_marker!("RISCV_SYSCALL_ROUNDTRIP_OK nr={}", tframe.syscall_num());
            early_marker!("RISCV_USER_RESUMED tid={} pc=0x{:x}", resume_tid, pc_final);
        }
    }

    // Resume through a pointer DERIVED FROM the live `&mut frame`, NOT the raw
    // `frame_ptr` argument. This is load-bearing for correctness, not cosmetic:
    // every register write-back above (PC/SP, mirrored GPRs, and the fresh-task
    // ABI lanes A0..A5) is performed through `frame`, a `&mut *frame_ptr` whose
    // `noalias` provenance lets LLVM assume no access through an *unrelated*
    // pointer observes those stores. Handing the extern `yarm_riscv64_trap_return`
    // the raw `frame_ptr` (a separate provenance) is exactly such an unrelated
    // access, so LLVM dead-stripped the A0..A5 stores and fresh tasks resumed with
    // a0..a5 = 0 (PM booted with zero startup caps → PM_NO_RECV_CAP, stalling the
    // whole RISC-V service chain). Reborrowing the pointer from `frame` keeps a
    // single provenance chain (`frame_ptr → &mut frame → *const derived here`), so
    // the extern read is modeled as observing the write-back and the stores
    // survive optimization on their own — independent of any logging.
    let frame_resume: *const RiscvTrapFrame = frame;
    unsafe { yarm_riscv64_trap_return(frame_resume) }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn riscv_trap_halt(reason: &'static str) -> ! {
    early_marker!("RISCV_TRAP_HALTED reason={}", reason);
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

// Stage 197A removed the synthetic `initramfs_static_hello_world_elf` fallback and the
// `load_init_elf_from_initramfs_vfs` Option-collapsing loader. The required `/init` ELF is now
// loaded via the arch-neutral `crate::kernel::boot::load_required_init_elf_bytes()`, which
// distinguishes every fatal reason; a failure halts boot with an explicit `BOOT_FATAL_*` marker.

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
const INITRD_INIT_ELF_MAX_SIZE: usize = 16 * 1024 * 1024;

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
    // Stage 197A: the `/init` ELF is MANDATORY — no synthetic fallback. Any load failure halts
    // boot with an explicit `BOOT_FATAL_*` diagnostic via the shared per-arch fatal-halt path.
    if crate::kernel::boot::force_init_zc_load_fail() {
        crate::yarm_log!("BOOT_FATAL_INIT_ZC_LOAD_FAILED reason=fault_injection");
        panic!("BOOT_FATAL_INIT_ZC_LOAD_FAILED: forced init load fault injection");
    }
    let init_owned = match crate::kernel::boot::load_required_init_elf_bytes() {
        Ok(bytes) => bytes,
        Err(reason) => {
            crate::kernel::boot::log_init_load_fatal(reason);
            panic!("init load fatal: {:?}", reason);
        }
    };
    let init_bytes: &[u8] = &init_owned;
    let init_source = "initrd";
    let init_elf_info = match yarm_srv_common::elf::ElfImageInfo::parse(0, init_bytes) {
        Ok(info) => info,
        Err(e) => {
            crate::yarm_log!(
                "BOOT_FATAL_INIT_ELF_INVALID reason=parse_header err={:?}",
                e
            );
            panic!("BOOT_FATAL_INIT_ELF_INVALID: {:?}", e);
        }
    };
    let (_, init_first_pt_load, init_heap) =
        match kernel.load_elf_pt_load_segments(init_asid, init_bytes) {
            Ok(v) => v,
            Err(e) => {
                crate::yarm_log!("BOOT_FATAL_INIT_ZC_LOAD_FAILED reason=pt_load err={:?}", e);
                panic!("BOOT_FATAL_INIT_ZC_LOAD_FAILED: {:?}", e);
            }
        };
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
    let sup_ctrl_send_init = if supervisor_aei.is_some() {
        let c = kernel.grant_capability_task_to_task_with_rights(
            0,
            sup_ctrl_send_root,
            RING3_INIT_SERVER_TID,
            crate::kernel::capabilities::CapRights::SEND,
        )?;
        crate::yarm_log!(
            "CAP_GRANT_BOOT dst_tid={} slot=4 cap={} rights=SEND result=ok",
            RING3_INIT_SERVER_TID,
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
    if let Some(c) = sup_ctrl_send_init {
        init_args[4] = c.0;
    }
    init_args[9] = RING3_SUPERVISOR_TID;
    // Stage 159BC/D: knob-gated (`yarm.ipc_recv_proof=1`) IPC recv-v2 oracle
    // loopback. Slots 6/7 (init_alert_send/recv, unused by init's bootstrap) are
    // populated ONLY when the proof knob is set; a normal boot leaves them zero
    // and init runs byte-identically.
    if let Some((proof_send_cap, proof_recv_cap)) =
        crate::kernel::boot::provision_init_ipc_recv_proof_loopback(kernel, RING3_INIT_SERVER_TID)
    {
        init_args[6] = proof_send_cap as u64;
        init_args[7] = proof_recv_cap as u64;
    }
    // Stage 163: sub-knob-gated (`yarm.ipc_recv_proof_sender_wake=1`) coordination
    // endpoint E2 recv cap in slot 13 (service_extra_cap_0, unused by init).
    if let Some(e2_recv_cap) = crate::kernel::boot::provision_init_ipc_recv_proof_sender_wake_e2(
        kernel,
        RING3_INIT_SERVER_TID,
    ) {
        init_args[13] = e2_recv_cap as u64;
        // Stage 163A: communicate E1's buffered capacity (slot 14, service_extra_cap_1)
        // so init fills E1 to exactly full with non-blocking sends and never blocks.
        init_args[14] = crate::kernel::boot::IPC_RECV_PROOF_E1_DEPTH as u64;
    }
    // Stage 196C/196D/196E/196F: default-off RISC-V oracle WORKLOADS reuse init slot 5
    // (supervisor_control_recv_ep, unused by init on RISC-V) as a sentinel: 1 = FutexWake live
    // oracle (196C); 2 = queue-switch context-switch FOUNDATION oracle (196D); 3 = FutexWait
    // SWITCH oracle workload (196E/196F); 4 = FutexWait no-incoming IDLE oracle workload (196F).
    // A normal boot leaves it 0 and init skips all four. NB: these are WORKLOAD selectors only —
    // the FutexWait retirement mechanism itself is DEFAULT-ON (no knob) as of 196F.
    if crate::kernel::boot::riscv_yield_lone_task_oracle_enabled() {
        init_args[5] = 6;
        crate::yarm_log!("RISCV_YIELD_LONE_TASK_ORACLE_PROVISION_OK slot5=6");
    } else if crate::kernel::boot::riscv_yield_two_task_oracle_enabled() {
        init_args[5] = 5;
        crate::yarm_log!("RISCV_YIELD_TWO_TASK_ORACLE_PROVISION_OK slot5=5");
    } else if crate::kernel::boot::riscv_futex_wait_idle_oracle_enabled() {
        init_args[5] = 4;
        crate::yarm_log!("RISCV_FUTEX_WAIT_IDLE_ORACLE_PROVISION_OK slot5=4");
    } else if crate::kernel::boot::riscv_futex_wait_oracle_enabled() {
        init_args[5] = 3;
        crate::yarm_log!("RISCV_FUTEX_WAIT_ORACLE_PROVISION_OK slot5=3");
    } else if crate::kernel::boot::riscv_queue_switch_foundation_oracle_enabled() {
        init_args[5] = 2;
        crate::yarm_log!("RISCV_QUEUE_SWITCH_FOUNDATION_PROVISION_OK slot5=2");
    } else if crate::kernel::boot::riscv_futex_wake_oracle_enabled() {
        init_args[5] = 1;
        crate::yarm_log!("RISCV_FUTEX_WAKE_ORACLE_PROVISION_OK slot5=1");
    }
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
    // Use the OpenSBI-reported boot-hart id captured in _start; the legacy
    // BOOTSTRAP_CPU_ID constant was wrong for any nonzero boot hart.
    let boot_hart = boot_hart_id();
    let mut parked_count: usize = 0;
    for hart_id in 0..QEMU_VIRT_HSM_SECONDARY_HART_LIMIT {
        if hart_id == boot_hart {
            continue;
        }
        let slot = hart_id;
        let handoff_ptr = prepare_secondary_handoff(slot, hart_id);
        match crate::arch::riscv64::sbi::hsm_hart_start(hart_id, entry_addr, handoff_ptr) {
            Ok(()) => {
                let acked = wait_for_secondary_ack(slot);
                if acked {
                    parked_count = parked_count.saturating_add(1);
                }
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
    early_marker!("RISCV_SECONDARY_HARTS_PARKED count={}", parked_count);
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
    // Stage 196A: own KernelState through a boot-constructed `SharedKernel`
    // (the same Stage-2N shared trap path x86_64/AArch64 use), so the S-mode
    // trap bridge can route through the RISC-V shared wrapper's bounded
    // `with_cpu` broad-lock phase + post-lock drain instead of a persistent
    // raw `&'static mut KernelState`. `init_shared_static` writes the
    // canonical `SharedKernel` into BOOTSTRAP_SHARED_KERNEL (.bss, inside the
    // kernel-shared Sv39 gigapage), like `init_static` did for
    // BOOTSTRAP_KERNEL_STATE — the heap-boxing OOM path (§5) is still avoided.
    let shared = crate::kernel::boot::Bootstrap::init_shared_static().expect("kernel init");
    // SAFETY: single boot hart; no trap handler can race before
    // install_riscv_trap_shared_kernel stores the pointer, and this raw boot
    // borrow is used only for the non-returning boot/dispatch sequence below
    // (it is never used again after the first `sret` into user space, at which
    // point the trap bridge takes over via `shared.with_cpu`).
    let kernel: &mut crate::kernel::boot::KernelState = unsafe { shared.borrow_kernel_for_boot() };
    // Install the SharedKernel pointer for the S-mode trap bridge before any
    // user task runs. BOOTSTRAP_SHARED_KERNEL lives in .bss, so the pointer
    // remains valid for the life of the kernel.
    install_riscv_trap_shared_kernel(shared);
    crate::yarm_log!("YARM_LOCK_SPLIT_STAGE196A_INSTALLED arch=riscv64 shared=1 raw=0");
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
    // Discover and stage the present-hart bitmap so YARM_BOOT_OK reports the
    // real OpenSBI topology, not the conservative single-hart fallback.
    stage_riscv64_present_cpu_bitmap(dtb);
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

    // Save the DTB slice for late consumers (PLIC discovery, future
    // interrupt-source enumeration). The slice is `'static` because it
    // points at firmware-owned memory that the bootloader does not
    // recycle for the lifetime of the kernel.
    save_dtb_for_late_consumers(dtb);

    // Stage the real RAM window and reserve firmware/DTB/initrd so the frame
    // allocator never hands out MMIO or firmware memory. Without this the
    // common fallback memory map (NEXT_ANON_PHYS_BASE = 0x1000_0000, the QEMU
    // virt UART region, which is BELOW RAM at 0x8000_0000) seeded the
    // allocators with MMIO addresses, producing a store access fault when the
    // first frame was written.
    stage_riscv64_boot_memory(dtb, start_info_ptr);
}

/// Discovers present harts from the FDT `/cpus` node, stages the bitmap
/// for the scheduler, and emits the topology breadcrumbs the smoke gate
/// pins. Online CPUs remain 1 (BSP-only) until RISC-V SMP scheduling
/// lands; the breadcrumb explicitly records that.
///
/// The binary FDT walker (`arch::fdt::cpus_hart_id_bitmap`) is consulted
/// directly here -- not through the generic `topology::discover_present_cpu_bitmap`
/// fallback chain -- so a malformed or empty `/cpus` node is reported with
/// an explicit `RISCV_DTB_CPU_SCAN_FAILED` breadcrumb instead of silently
/// returning the single-hart default. The smoke gate rejects that silent
/// fallback under `-smp >1`.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn stage_riscv64_present_cpu_bitmap(dtb: &'static [u8]) {
    early_marker!("RISCV_DTB_CPU_SCAN_BEGIN dtb=0x{:x}", dtb.as_ptr() as usize);
    let bitmap = match crate::arch::fdt::cpus_hart_id_bitmap(dtb) {
        Some(bitmap) if bitmap != 0 => {
            emit_dtb_cpu_node_markers(bitmap);
            early_marker!(
                "RISCV_DTB_CPU_SCAN_DONE bitmap=0x{:x} count={}",
                bitmap,
                bitmap.count_ones()
            );
            bitmap
        }
        Some(_) => {
            early_marker!("RISCV_DTB_CPU_SCAN_FAILED reason=no_cpus_node_found");
            crate::arch::riscv64::topology::default_present_cpu_bitmap()
        }
        None => {
            early_marker!("RISCV_DTB_CPU_SCAN_FAILED reason=malformed_fdt");
            crate::arch::riscv64::topology::default_present_cpu_bitmap()
        }
    };
    let _ = crate::arch::boot_entry::stage_present_cpu_bitmap_for_bootstrap(bitmap);
    let boot_hart = boot_hart_id();
    emit_present_hart_markers(bitmap);
    early_marker!(
        "RISCV_HART_TOPOLOGY present_cpus={} present_bitmap=0x{:x} boot_hart={}",
        bitmap.count_ones(),
        bitmap,
        boot_hart
    );
    crate::yarm_log!(
        "RISCV_SCHEDULER_BSP_ONLY online_cpus=1 reason=riscv_smp_scheduler_not_enabled"
    );
    // Live multi-hart IRQ delivery is not validated in this build. The
    // smoke gate accepts live timer + PLIC under -smp 1; for present_cpus
    // > 1 we explicitly record the deferral so a regression cannot
    // silently widen the IRQ scope.
    if bitmap.count_ones() > 1 {
        crate::yarm_log!(
            "RISCV_IRQ_SMP_TOPOLOGY_DEFERRED reason=present_topology_not_live_validated"
        );
    }
}

/// Emits one `RISCV_DTB_CPU_NODE hart=N` marker per hart bit set, in the
/// scan-result bitmap returned by the binary FDT walker.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn emit_dtb_cpu_node_markers(bitmap: u64) {
    let mut hart_id = 0u32;
    let mut remaining = bitmap;
    while remaining != 0 {
        if remaining & 1 != 0 {
            early_marker!("RISCV_DTB_CPU_NODE hart={}", hart_id);
        }
        remaining >>= 1;
        hart_id += 1;
    }
}

/// Emits one `RISCV_HART_PRESENT hart=N` marker per hart bit set in the
/// staged present-CPU bitmap.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn emit_present_hart_markers(bitmap: u64) {
    let mut hart_id = 0u32;
    let mut remaining = bitmap;
    while remaining != 0 {
        if remaining & 1 != 0 {
            early_marker!("RISCV_HART_PRESENT hart={}", hart_id);
        }
        remaining >>= 1;
        hart_id += 1;
    }
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
