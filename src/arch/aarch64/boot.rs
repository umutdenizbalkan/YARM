// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
use core::arch::global_asm;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
use core::fmt::Write;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
use core::sync::atomic::{AtomicU8, Ordering};

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
global_asm!(
    r#"
    .section .bss.bootstack,"aw",@nobits
    .align 16
boot_stack_aarch64:
    .skip 0x01000000
boot_stack_aarch64_end:
    .align 16
exc_stack_aarch64:
    .skip 65536
exc_stack_aarch64_end:
    .align 16
secondary_boot_stacks:
    .skip 0x00100000
secondary_boot_stacks_end:

    .section .text.boot,"ax",@progbits
    .weak _start
    .type _start,%function
_start:
    mov x20, x0
    adrp x1, __bss_start
    add x1, x1, :lo12:__bss_start
    adrp x2, __bss_end
    add x2, x2, :lo12:__bss_end
    sub x2, x2, x1
    cbz x2, 2f
1:
    str xzr, [x1], #8
    subs x2, x2, #8
    b.gt 1b
2:
    adrp x0, boot_stack_aarch64_end
    add x0, x0, :lo12:boot_stack_aarch64_end
    mov sp, x0
    bl yarm_aarch64_boot_breadcrumb_b0
    bl yarm_aarch64_boot_marker_start
    bl yarm_aarch64_boot_breadcrumb_b1
    bl yarm_aarch64_enter_el1_if_needed
    bl yarm_aarch64_boot_breadcrumb_b2
    bl yarm_aarch64_enable_fp_simd
    bl yarm_aarch64_boot_breadcrumb_b3
    mov x0, x20
    .weak yarm_kernel_main
    bl yarm_kernel_main
3:
    wfe
    b 3b

    .global yarm_aarch64_enter_el1_if_needed
    .type yarm_aarch64_enter_el1_if_needed,%function
yarm_aarch64_enter_el1_if_needed:
    mrs x0, CurrentEL
    lsr x0, x0, #2
    cmp x0, #0x2
    b.ne 2f
    // Do not inherit EL2 trap/control bits from reset/firmware state.
    // Program a known baseline: EL1 runs AArch64 (RW=1), everything else clear.
    mov x1, #(1 << 31)
    msr HCR_EL2, x1
    // Clear common EL2 trap controls so EL1 sysreg/MMU setup does not trap
    // unexpectedly during early bring-up.
    msr CPTR_EL2, xzr
    msr HSTR_EL2, xzr
    msr MDCR_EL2, xzr
    mrs x1, CNTHCTL_EL2
    orr x1, x1, #3
    msr CNTHCTL_EL2, x1
    msr CNTVOFF_EL2, xzr
    mov x1, #(3 << 20)
    msr CPACR_EL1, x1
    ldr x1, =0x30D00800
    msr SCTLR_EL1, x1
    isb
    mov x2, sp
    msr SP_EL1, x2
    adr x1, 1f
    msr ELR_EL2, x1
    mov x1, #0x3C5
    msr SPSR_EL2, x1
    isb
    eret
1:
2:
    ret
    "#
);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
global_asm!(
    r#"
    .section .text.boot,"ax",@progbits
    .global yarm_aarch64_secondary_entry
    .type yarm_aarch64_secondary_entry,%function
yarm_aarch64_secondary_entry:
    mrs x0, MPIDR_EL1
    and x0, x0, #0xff
    adrp x1, secondary_boot_stacks_end
    add x1, x1, :lo12:secondary_boot_stacks_end
    mov x2, #0x4000
    mul x3, x0, x2
    sub sp, x1, x3
    bl yarm_aarch64_secondary_cpu_boot
1:
    wfe
    b 1b
    "#
);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
global_asm!(
    r#"
    .section .text.boot,"ax",@progbits
    .global yarm_aarch64_enter_user_mode_eret
    .type yarm_aarch64_enter_user_mode_eret,%function
yarm_aarch64_enter_user_mode_eret:
    mov x9, x0
    mov x10, x1
    mov x11, x2
    mov x12, x3
    mov x13, x4
    msr sp_el0, x10
    msr tpidr_el0, x13
    msr elr_el1, x9
    msr spsr_el1, xzr
    mov x0, x11
    mov x1, x12
    eret
    "#
);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_PTE_VALID: u64 = 1 << 0;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_PTE_TABLE: u64 = 1 << 1;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_PTE_AF: u64 = 1 << 10;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_PTE_SH_INNER: u64 = 0b11 << 8;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_PTE_ATTR_SHIFT: u64 = 2;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_ATTRIDX_NORMAL_WB: u64 = 0;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_ATTRIDX_DEVICE_NGNRE: u64 = 3;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_BLOCK_2M: u64 = 2 * 1024 * 1024;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_BLOCK_1G: u64 = 1024 * 1024 * 1024;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const AARCH64_UART_MMIO_BASE: u64 = 0x0900_0000;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[repr(C, align(4096))]
struct AlignedL2([u64; 512]);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[repr(C, align(4096))]
struct AlignedL1([u64; 512]);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
static mut BOOT_L1_TABLE: AlignedL1 = AlignedL1([0; 512]);
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
static mut BOOT_L2_TABLE: AlignedL2 = AlignedL2([0; 512]);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
global_asm!(
    r#"
    .section .text.boot,"ax",@progbits
    .align 11
    .global yarm_aarch64_vector_table_el1
    .type yarm_aarch64_vector_table_el1,%function
yarm_aarch64_vector_table_el1:
    // Current EL with SP0 (offsets 0x000..0x180)
    .balign 128
    b yarm_aarch64_vector_sync_lower_a64_sp0
    .balign 128
    b yarm_aarch64_vector_irq_lower_a64_sp0
    .balign 128
    b yarm_aarch64_vector_fiq_lower_a64_sp0
    .balign 128
    b yarm_aarch64_vector_serror_lower_a64_sp0
    // Current EL with SPx (offsets 0x200..0x380)
    .balign 128
    b yarm_aarch64_vector_sync_current
    .balign 128
    b yarm_aarch64_vector_irq_current
    .balign 128
    b yarm_aarch64_vector_fiq_current
    .balign 128
    b yarm_aarch64_vector_serror_current
    // Lower EL using AArch64 (offsets 0x400..0x580)
    .balign 128
    b yarm_aarch64_vector_sync_lower_a64
    .balign 128
    b yarm_aarch64_vector_irq_lower_a64
    .balign 128
    b yarm_aarch64_vector_fiq_lower_a64
    .balign 128
    b yarm_aarch64_vector_serror_lower_a64
    // Lower EL using AArch32 (offsets 0x600..0x780)
    .balign 128
    b yarm_aarch64_vector_sync_lower_a32
    .balign 128
    b yarm_aarch64_vector_irq_lower_a32
    .balign 128
    b yarm_aarch64_vector_fiq_lower_a32
    .balign 128
    b yarm_aarch64_vector_serror_lower_a32
    .balign 128

    .macro YARM_AARCH64_VECTOR_STUB name kind
    .global \name
    .type \name,%function
\name:
    mov x0, #\kind
    b yarm_aarch64_vector_dispatch
    .endm

    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_sync_lower_a64_sp0, 1
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_irq_lower_a64_sp0, 2
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_fiq_lower_a64_sp0, 3
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_serror_lower_a64_sp0, 4
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_sync_current, 5
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_irq_current, 6
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_fiq_current, 7
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_serror_current, 8
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_sync_lower_a64, 9
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_irq_lower_a64, 10
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_fiq_lower_a64, 11
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_serror_lower_a64, 12
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_sync_lower_a32, 13
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_irq_lower_a32, 14
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_fiq_lower_a32, 15
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_serror_lower_a32, 16

    .global yarm_aarch64_vector_dispatch
    .type yarm_aarch64_vector_dispatch,%function
yarm_aarch64_vector_dispatch:
    // Per-task EL1 stack frame layout (800 bytes total):
    //   0x000..0x0F7 : x0..x30 (31 x 8 bytes)
    //   0x0F8        : SP_EL0
    //   0x100        : ELR_EL1
    //   0x108        : SPSR_EL1
    //   0x110        : ESR_EL1
    //   0x118        : FAR_EL1
    //   0x120..0x31F : q0..q31 (32 x 16 bytes)
    sub sp, sp, #800
    stp x0, x1, [sp, #0]
    stp x2, x3, [sp, #16]
    stp x4, x5, [sp, #32]
    stp x6, x7, [sp, #48]
    stp x8, x9, [sp, #64]
    stp x10, x11, [sp, #80]
    stp x12, x13, [sp, #96]
    stp x14, x15, [sp, #112]
    stp x16, x17, [sp, #128]
    stp x18, x19, [sp, #144]
    stp x20, x21, [sp, #160]
    stp x22, x23, [sp, #176]
    stp x24, x25, [sp, #192]
    stp x26, x27, [sp, #208]
    stp x28, x29, [sp, #224]
    str x30, [sp, #240]
    mrs x9, sp_el0
    str x9, [sp, #248]
    mrs x9, elr_el1
    str x9, [sp, #256]
    mrs x9, spsr_el1
    str x9, [sp, #264]
    mrs x9, esr_el1
    str x9, [sp, #272]
    mrs x9, far_el1
    str x9, [sp, #280]
    stp q0, q1, [sp, #288]
    stp q2, q3, [sp, #320]
    stp q4, q5, [sp, #352]
    stp q6, q7, [sp, #384]
    stp q8, q9, [sp, #416]
    stp q10, q11, [sp, #448]
    stp q12, q13, [sp, #480]
    stp q14, q15, [sp, #512]
    stp q16, q17, [sp, #544]
    stp q18, q19, [sp, #576]
    stp q20, q21, [sp, #608]
    stp q22, q23, [sp, #640]
    stp q24, q25, [sp, #672]
    stp q26, q27, [sp, #704]
    stp q28, q29, [sp, #736]
    stp q30, q31, [sp, #768]
    msr daifset, #0xf
    mov x1, sp
    bl yarm_aarch64_vector_entry
    ldp q0, q1, [sp, #288]
    ldp q2, q3, [sp, #320]
    ldp q4, q5, [sp, #352]
    ldp q6, q7, [sp, #384]
    ldp q8, q9, [sp, #416]
    ldp q10, q11, [sp, #448]
    ldp q12, q13, [sp, #480]
    ldp q14, q15, [sp, #512]
    ldp q16, q17, [sp, #544]
    ldp q18, q19, [sp, #576]
    ldp q20, q21, [sp, #608]
    ldp q22, q23, [sp, #640]
    ldp q24, q25, [sp, #672]
    ldp q26, q27, [sp, #704]
    ldp q28, q29, [sp, #736]
    ldp q30, q31, [sp, #768]
    ldr x9, [sp, #248]
    msr sp_el0, x9
    ldr x9, [sp, #256]
    msr elr_el1, x9
    ldr x9, [sp, #264]
    msr spsr_el1, x9
    ldr x30, [sp, #240]
    ldp x28, x29, [sp, #224]
    ldp x26, x27, [sp, #208]
    ldp x24, x25, [sp, #192]
    ldp x22, x23, [sp, #176]
    ldp x20, x21, [sp, #160]
    ldp x18, x19, [sp, #144]
    ldp x16, x17, [sp, #128]
    ldp x14, x15, [sp, #112]
    ldp x12, x13, [sp, #96]
    ldp x10, x11, [sp, #80]
    ldp x8, x9, [sp, #64]
    ldp x6, x7, [sp, #48]
    ldp x4, x5, [sp, #32]
    ldp x2, x3, [sp, #16]
    ldp x0, x1, [sp, #0]
    add sp, sp, #800
    eret
    "#
);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_boot_marker_start() {
    crate::arch::aarch64::console::init_early_mmio_base(0x0900_0000);
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BOOT_MARKER stage=_start");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_boot_breadcrumb_b0() {
    crate::arch::aarch64::console::init_early_mmio_base(0x0900_0000);
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB B0");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_boot_breadcrumb_b1() {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB B1");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_boot_breadcrumb_b2() {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB B2");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_boot_breadcrumb_b3() {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB B3");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_enable_fp_simd() {
    unsafe {
        let mut cpacr_el1: u64;
        core::arch::asm!("mrs {0}, CPACR_EL1", out(reg) cpacr_el1, options(nomem, preserves_flags));
        cpacr_el1 |= (0b11u64) << 20;
        core::arch::asm!("msr CPACR_EL1, {0}", in(reg) cpacr_el1, options(nomem, preserves_flags));
        core::arch::asm!("isb", options(nomem, preserves_flags));
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
static TRAP_KERNEL_STATE_PTR: core::sync::atomic::AtomicPtr<crate::kernel::boot::KernelState> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PsciConduit {
    Unknown = 0,
    Smc = 1,
    Hvc = 2,
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
static PSCI_CONDUIT: AtomicU8 = AtomicU8::new(PsciConduit::Unknown as u8);

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn install_trap_kernel_state(kernel: &mut crate::kernel::boot::KernelState) {
    TRAP_KERNEL_STATE_PTR.store(kernel as *mut _, core::sync::atomic::Ordering::SeqCst);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn trap_kernel_state_mut() -> Option<&'static mut crate::kernel::boot::KernelState> {
    let ptr = TRAP_KERNEL_STATE_PTR.load(core::sync::atomic::Ordering::SeqCst);
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { &mut *ptr })
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[repr(C, align(16))]
struct Aarch64VectorFrame {
    gprs: [u64; 31],
    sp_el0: u64,
    elr_el1: u64,
    spsr_el1: u64,
    esr_el1: u64,
    far_el1: u64,
    neon: [[u8; 16]; 32],
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn write_trapframe_back_to_vector_frame(
    frame: &mut Aarch64VectorFrame,
    trap_frame: &crate::kernel::trapframe::TrapFrame,
) {
    for idx in 0..31 {
        frame.gprs[idx] = trap_frame.user_gpr(idx) as u64;
    }
    frame.sp_el0 = trap_frame.saved_sp() as u64;
    frame.elr_el1 = trap_frame.saved_pc() as u64;
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_vector_entry(kind: u64, frame: *mut Aarch64VectorFrame) {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_VECTOR_ENTRY");
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BOOT_MARKER stage=exception");
    let Some(frame) = (unsafe { frame.as_mut() }) else {
        crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_FRAME missing");
        return;
    };
    struct FixedBufWriter<'a> {
        buf: &'a mut [u8],
        len: usize,
    }
    impl Write for FixedBufWriter<'_> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let bytes = s.as_bytes();
            let remaining = self.buf.len().saturating_sub(self.len);
            let copy_len = remaining.min(bytes.len());
            self.buf[self.len..self.len + copy_len].copy_from_slice(&bytes[..copy_len]);
            self.len += copy_len;
            Ok(())
        }
    }
    let mut line = [0u8; 160];
    let mut writer = FixedBufWriter {
        buf: &mut line,
        len: 0,
    };
    let _ = write!(
        writer,
        "YARM_AARCH64_EXCEPTION_REGS esr_el1=0x{:016x} far_el1=0x{:016x} elr_el1=0x{:016x} spsr_el1=0x{:016x}",
        frame.esr_el1, frame.far_el1, frame.elr_el1, frame.spsr_el1
    );
    let line_len = writer.len;
    if let Ok(msg) = core::str::from_utf8(&line[..line_len]) {
        crate::arch::aarch64::console::write_line(msg);
    }
    let is_irq_kind = matches!(kind, 2 | 6 | 10 | 14);
    if let Some(kernel) = trap_kernel_state_mut() {
        let mut trap_frame = crate::kernel::trapframe::TrapFrame::zeroed();
        trap_frame.set_saved_pc(frame.elr_el1 as usize);
        trap_frame.set_saved_sp(frame.sp_el0 as usize);
        for idx in 0..31 {
            trap_frame.set_user_gpr(idx, frame.gprs[idx] as usize);
        }
        let context = crate::arch::aarch64::trap::Aarch64TrapContext {
            esr_el1: frame.esr_el1 as u32,
            far_el1: frame.far_el1,
            irq_line: None,
            is_timer_irq: is_irq_kind,
        };
        if crate::arch::aarch64::trap::handle_trap_entry(
            kernel,
            crate::kernel::scheduler::CpuId(0),
            context,
            Some(&mut trap_frame),
        )
        .is_ok()
        {
            write_trapframe_back_to_vector_frame(frame, &trap_frame);
        } else {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_TRAP_HANDLE failed");
        }
    } else {
        crate::arch::aarch64::console::write_line("YARM_AARCH64_TRAP_HANDLE no_kernel_state");
    }
    match kind {
        1 => crate::arch::aarch64::console::write_line(
            "YARM_AARCH64_EXCEPTION_KIND sync_current_sp0",
        ),
        2 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND irq_current_sp0")
        }
        3 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND fiq_current_sp0")
        }
        4 => crate::arch::aarch64::console::write_line(
            "YARM_AARCH64_EXCEPTION_KIND serr_current_sp0",
        ),
        5 => crate::arch::aarch64::console::write_line(
            "YARM_AARCH64_EXCEPTION_KIND sync_current_spx",
        ),
        6 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND irq_current_spx")
        }
        7 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND fiq_current_spx")
        }
        8 => crate::arch::aarch64::console::write_line(
            "YARM_AARCH64_EXCEPTION_KIND serr_current_spx",
        ),
        9 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND sync_lower_a64")
        }
        10 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND irq_lower_a64")
        }
        11 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND fiq_lower_a64")
        }
        12 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND serr_lower_a64")
        }
        13 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND sync_lower_a32")
        }
        14 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND irq_lower_a32")
        }
        15 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND fiq_lower_a32")
        }
        16 => {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND serr_lower_a32")
        }
        _ => crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND unknown"),
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const RING3_INIT_SERVER_TID: u64 = 1;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const INITRAMFS_HELLO_WORLD_IMAGE_ID: u64 = 0x494E_4954_4845_4C4F; // "INITHELO"

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn initramfs_static_hello_world_elf() -> [u8; 256] {
    let mut image = [0u8; 256];
    // ELF header.
    image[..4].copy_from_slice(b"\x7FELF");
    image[4] = 2; // ELFCLASS64
    image[5] = 1; // little-endian
    image[6] = 1; // EV_CURRENT
    image[7] = 0; // SYSV ABI
    image[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    image[18..20].copy_from_slice(&0xB7u16.to_le_bytes()); // EM_AARCH64
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

    // Minimal "hello world" init image code stub:
    // movz x8,#0 ; svc #0 ; b svc
    image[128..140].copy_from_slice(&[
        0x08, 0x00, 0x80, 0xD2, 0x01, 0x00, 0x00, 0xD4, 0xFF, 0xFF, 0xFF, 0x17,
    ]);
    image
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
pub fn bootstrap_first_user_task(
    kernel: &mut crate::kernel::boot::KernelState,
) -> Result<(), crate::kernel::boot::KernelError> {
    use crate::kernel::boot::UserImageSpec;
    use crate::kernel::task::TaskClass;

    if kernel.task_asid(RING3_INIT_SERVER_TID).is_some() {
        return Ok(());
    }

    let (asid, _aspace_cap) = kernel.create_user_address_space()?;
    let image = initramfs_static_hello_world_elf();
    let entry = kernel.load_elf_pt_load_segments(asid, &image)?;
    kernel.spawn_user_task_from_image(UserImageSpec {
        tid: RING3_INIT_SERVER_TID,
        entry,
        asid: Some(asid),
        class: TaskClass::SystemServer,
    })?;
    crate::yarm_log!(
        "YARM_INIT_DONE arch=aarch64 phase=kernel_static_init_elf image_id=0x{:x} seeded=0 initramfs_handled=1 devfs_handled=0",
        INITRAMFS_HELLO_WORLD_IMAGE_ID
    );
    Ok(())
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "aarch64")))]
pub fn bootstrap_first_user_task(
    _kernel: &mut crate::kernel::boot::KernelState,
) -> Result<(), crate::kernel::boot::KernelError> {
    Ok(())
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
pub fn enter_dispatched_user_task_if_available(
    kernel: &crate::kernel::boot::KernelState,
    dispatched_tid: Option<u64>,
) {
    if let Some(tid) = dispatched_tid
        && let Some(context) = kernel.thread_user_context(tid)
        && context.instruction_ptr.0 != 0
        && context.stack_ptr.0 != 0
    {
        let tls = kernel.thread_tls_base(tid).unwrap_or(0) as u64;
        crate::yarm_log!(
            "YARM_AARCH64_RING3_INIT_TASK tid={} entry=0x{:x} stack_top=0x{:x} tls=0x{:x}",
            tid,
            context.instruction_ptr.0,
            context.stack_ptr.0,
            tls
        );
        unsafe {
            unsafe extern "C" {
                fn yarm_aarch64_enter_user_mode_eret(
                    entry: u64,
                    stack_top: u64,
                    arg0: u64,
                    arg1: u64,
                    tls: u64,
                ) -> !;
            }
            yarm_aarch64_enter_user_mode_eret(
                context.instruction_ptr.0,
                context.stack_ptr.0,
                context.arg0 as u64,
                context.arg1 as u64,
                tls,
            );
        }
    }
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "aarch64")))]
pub fn enter_dispatched_user_task_if_available(
    _kernel: &crate::kernel::boot::KernelState,
    _dispatched_tid: Option<u64>,
) {
}

pub fn run_with_prepared_kernel(run: fn(&mut crate::kernel::boot::KernelState)) {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    crate::arch::aarch64::console::write_line(
        "YARM_AARCH64_BOOT_MARKER stage=run_with_prepared_kernel",
    );
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    let saved_ttbr0: u64 = {
        let ttbr0: u64;
        unsafe {
            core::arch::asm!(
                "mrs {0}, ttbr0_el1",
                out(reg) ttbr0,
                options(nostack, preserves_flags)
            );
        }
        ttbr0
    };
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    crate::arch::aarch64::console::write_line(
        "YARM_AARCH64_BOOT_MARKER stage=bootstrap_init_begin",
    );
    let kernel = crate::kernel::boot::Bootstrap::init_static().expect("kernel init");
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    install_trap_kernel_state(kernel);
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BOOT_MARKER stage=bootstrap_init_done");
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    let started_secondary = start_secondary_cpus(kernel);
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    if saved_ttbr0 != 0 {
        unsafe {
            core::arch::asm!(
                "msr ttbr0_el1, {0}",
                "isb",
                in(reg) saved_ttbr0,
                options(nostack, preserves_flags)
            );
        }
    }
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    {
        // Do not touch EL1 physical timer registers during early bootstrap.
        // On some boot paths (e.g. direct EL1 entry), CNTHCTL_EL2 may still
        // trap EL1 timer sysreg accesses, which raises a synchronous exception
        // right after this stage marker. Timer/IRQ bring-up is deferred to the
        // normal init path once platform control is fully established.
    }
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    crate::yarm_log!(
        "YARM_SMP_STARTUP started_secondary={} online_cpus={} present_cpus={}",
        started_secondary,
        kernel.online_cpu_count(),
        kernel.present_cpu_count()
    );
    crate::yarm_log!(
        "YARM_BOOT_OK present_cpus={} present_bitmap=0x{:x} online_cpus={}",
        kernel.present_cpu_count(),
        kernel.present_cpu_bitmap(),
        kernel.online_cpu_count()
    );
    run(kernel);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn start_secondary_cpus(kernel: &mut crate::kernel::boot::KernelState) -> usize {
    let mut started = 0usize;
    let secondary_entry = yarm_aarch64_secondary_entry as usize as u64;
    let conduit = match PSCI_CONDUIT.load(Ordering::Acquire) {
        x if x == PsciConduit::Smc as u8 => PsciConduit::Smc,
        x if x == PsciConduit::Hvc as u8 => PsciConduit::Hvc,
        _ => PsciConduit::Unknown,
    };
    if matches!(conduit, PsciConduit::Unknown) {
        crate::yarm_log!("YARM_SMP_PSCI unavailable_conduit");
        return 0;
    }
    let present = kernel.present_cpu_bitmap();
    for cpu_id in 0..64u8 {
        if cpu_id == crate::arch::platform_constants::BOOTSTRAP_CPU_ID {
            continue;
        }
        let mask = 1u64 << cpu_id;
        if (present & mask) == 0 {
            continue;
        }
        let cpu = crate::kernel::scheduler::CpuId(cpu_id);
        let psci_ret = psci_cpu_on(conduit, cpu.0 as u64, secondary_entry, cpu.0 as u64);
        crate::yarm_log!("YARM_SMP_PSCI cpu={} ret={}", cpu.0, psci_ret);
        if psci_ret == 0 && kernel.bring_up_cpu(cpu).is_ok() {
            started += 1;
        }
    }
    started
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
unsafe extern "C" {
    fn yarm_aarch64_secondary_entry() -> !;
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const PSCI_CPU_ON_FID: u64 = 0xC400_0003;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn psci_cpu_on(conduit: PsciConduit, target_cpu: u64, entry_point: u64, context_id: u64) -> i64 {
    let mut x0 = PSCI_CPU_ON_FID;
    let x1 = target_cpu;
    let x2 = entry_point;
    let x3 = context_id;
    unsafe {
        match conduit {
            PsciConduit::Smc => core::arch::asm!(
                "smc #0",
                inout("x0") x0,
                in("x1") x1,
                in("x2") x2,
                in("x3") x3,
                options(nostack)
            ),
            PsciConduit::Hvc => core::arch::asm!(
                "hvc #0",
                inout("x0") x0,
                in("x1") x1,
                in("x2") x2,
                in("x3") x3,
                options(nostack)
            ),
            PsciConduit::Unknown => return -1,
        }
    }
    x0 as i64
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_secondary_cpu_boot(cpu_id: u64) -> ! {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_SMP_SECONDARY_ONLINE");
    crate::yarm_log!("YARM_AARCH64_SMP_SECONDARY cpu={}", cpu_id);
    let cpu = crate::kernel::scheduler::CpuId(cpu_id as u8);
    unsafe {
        unsafe extern "C" {
            static yarm_aarch64_vector_table_el1: u8;
        }
        let vector_base = (&yarm_aarch64_vector_table_el1 as *const u8) as u64;
        core::arch::asm!("msr VBAR_EL1, {0}", in(reg) vector_base, options(nomem, preserves_flags));
        core::arch::asm!("isb", options(nomem, preserves_flags));
    }

    let kernel = loop {
        if let Some(kernel) = trap_kernel_state_mut() {
            break kernel;
        }
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    };
    let _ = kernel.set_current_cpu(cpu);
    let _ = kernel.process_cross_cpu_work_for_cpu(cpu);
    kernel.program_timer_deadline_current_cpu(
        crate::arch::platform_layout::BOOTSTRAP_TIMER_DEADLINE_TICKS,
    );
    crate::yarm_log!("YARM_AARCH64_SMP_SECONDARY_JOINED cpu={}", cpu_id);

    loop {
        let _ = kernel.set_current_cpu(cpu);
        let _ = kernel.process_cross_cpu_work_for_cpu(cpu);
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const MAX_PT_ALLOCATOR_REGIONS: usize = 8;

pub fn prepare_arch_boot(_start_info_ptr: usize) {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    {
        let start_info_ptr = _start_info_ptr;
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB P0");
        crate::yarm_log!(
            "YARM_AARCH64_START_INFO_PTR value=0x{:x}",
            start_info_ptr as u64
        );
        crate::arch::aarch64::console::write_line(
            "YARM_AARCH64_BOOT_MARKER stage=prepare_arch_boot",
        );
        unsafe {
            unsafe extern "C" {
                static yarm_aarch64_vector_table_el1: u8;
            }
            let vector_base = (&yarm_aarch64_vector_table_el1 as *const u8) as u64;
            core::arch::asm!("msr VBAR_EL1, {0}", in(reg) vector_base, options(nomem, preserves_flags));
            core::arch::asm!("isb", options(nomem, preserves_flags));
        }
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BOOT_MARKER stage=vbar_el1_ready");

        if let Some(dtb) = dtb_slice_from_start_info(start_info_ptr) {
            crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB P1");
            if let Some(parsed) = crate::arch::aarch64::dtb::parse_boot_dtb(dtb) {
                crate::yarm_log!(
                    "YARM_AARCH64_DTB memory_start=0x{:x} memory_len=0x{:x} initrd_start=0x{:x} initrd_end=0x{:x} gic_cpu_if_base=0x{:x}",
                    parsed.memory_start.unwrap_or(0),
                    parsed.memory_len.unwrap_or(0),
                    parsed.initrd_start.unwrap_or(0),
                    parsed.initrd_end.unwrap_or(0),
                    parsed.gic_cpu_if_base.unwrap_or(0),
                );
                if let Some(bitmap) = parsed.present_cpu_bitmap {
                    let _ = crate::arch::boot_entry::stage_present_cpu_bitmap_for_bootstrap(bitmap);
                    crate::yarm_log!(
                        "YARM_AARCH64_DTB_CPU_BITMAP value=0x{:x} count={}",
                        bitmap,
                        bitmap.count_ones()
                    );
                }
                let conduit = match parsed.psci_conduit {
                    crate::arch::aarch64::dtb::PsciConduit::Smc => PsciConduit::Smc,
                    crate::arch::aarch64::dtb::PsciConduit::Hvc => PsciConduit::Hvc,
                    crate::arch::aarch64::dtb::PsciConduit::Unknown => PsciConduit::Unknown,
                };
                PSCI_CONDUIT.store(conduit as u8, Ordering::Release);
                crate::yarm_log!("YARM_AARCH64_DTB_PSCI conduit={:?}", conduit);
                if let (Some(start), Some(len)) = (parsed.memory_start, parsed.memory_len) {
                    let _ = crate::arch::boot_entry::stage_detected_ram_for_bootstrap(&[
                        crate::kernel::frame_allocator::MemoryRegion {
                            start,
                            len,
                            usable: true,
                        },
                    ]);
                    let mut reserved: [(u64, u64); 5] = [(0, 0); 5];
                    let mut reserved_len = 0usize;
                    let page = crate::kernel::vm::PAGE_SIZE as u64;
                    let kernel_start = (core::ptr::addr_of!(__kernel_start) as u64) & !(page - 1);
                    let kernel_end = ((core::ptr::addr_of!(__kernel_end) as u64)
                        .saturating_add(page - 1))
                        & !(page - 1);
                    if kernel_end > kernel_start {
                        reserved[reserved_len] = (kernel_start, kernel_end);
                        reserved_len += 1;
                    }
                    let l1_start = (core::ptr::addr_of!(BOOT_L1_TABLE) as u64) & !(page - 1);
                    let l1_end = ((core::ptr::addr_of!(BOOT_L1_TABLE) as u64)
                        .saturating_add(core::mem::size_of::<AlignedL1>() as u64)
                        .saturating_add(page - 1))
                        & !(page - 1);
                    if l1_end > l1_start {
                        reserved[reserved_len] = (l1_start, l1_end);
                        reserved_len += 1;
                    }
                    let l2_start = (core::ptr::addr_of!(BOOT_L2_TABLE) as u64) & !(page - 1);
                    let l2_end = ((core::ptr::addr_of!(BOOT_L2_TABLE) as u64)
                        .saturating_add(core::mem::size_of::<AlignedL2>() as u64)
                        .saturating_add(page - 1))
                        & !(page - 1);
                    if l2_end > l2_start {
                        reserved[reserved_len] = (l2_start, l2_end);
                        reserved_len += 1;
                    }
                    let dtb_start = dtb.as_ptr() as u64;
                    let dtb_end = dtb_start.saturating_add(dtb.len() as u64);
                    if dtb_end > dtb_start {
                        reserved[reserved_len] = (dtb_start & !(page - 1), (dtb_end + (page - 1)) & !(page - 1));
                        reserved_len += 1;
                    }
                    if let (Some(initrd_start), Some(initrd_end)) = (parsed.initrd_start, parsed.initrd_end)
                        && initrd_end > initrd_start
                        && reserved_len < reserved.len()
                    {
                        reserved[reserved_len] = (
                            initrd_start & !(page - 1),
                            (initrd_end.saturating_add(page - 1)) & !(page - 1),
                        );
                        reserved_len += 1;
                    }
                    let (alloc_regions, alloc_regions_len) = build_allocator_regions_from_ram(
                        start,
                        len,
                        crate::arch::platform_layout::NEXT_ANON_PHYS_BASE,
                        &reserved[..reserved_len],
                    );
                    if alloc_regions_len > 0 {
                        let _ = crate::kernel::frame_allocator::init_pt_frame_allocator(
                            &alloc_regions[..alloc_regions_len],
                        );
                    }
                }
                if let Some(gic_base) = parsed.gic_cpu_if_base {
                    let mut desc = [0u8; 40];
                    if let Some(desc_len) = encode_irq_desc_gic_cpu_if_base(gic_base, &mut desc) {
                        let _ = crate::arch::boot_entry::stage_irq_controller_description_for_boot(
                            &desc[..desc_len],
                        );
                    }
                }
            }
        }
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB P2");
        setup_bootstrap_mmu();
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB P3");
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn build_allocator_regions_from_ram(
    ram_base: u64,
    ram_size: u64,
    alloc_start: u64,
    reserved: &[(u64, u64)],
) -> (
    [crate::kernel::frame_allocator::MemoryRegion; MAX_PT_ALLOCATOR_REGIONS],
    usize,
) {
    let mut out = [crate::kernel::frame_allocator::MemoryRegion {
        start: 0,
        len: 0,
        usable: false,
    }; MAX_PT_ALLOCATOR_REGIONS];
    let page = crate::kernel::vm::PAGE_SIZE as u64;
    let Some(ram_end) = ram_base.checked_add(ram_size) else {
        return (out, 0);
    };
    let start = align_up_u64(ram_base.max(alloc_start), page);
    let end = align_down_u64(ram_end, page);
    if end <= start {
        return (out, 0);
    }

    let mut segments = [(0u64, 0u64); MAX_PT_ALLOCATOR_REGIONS];
    let mut seg_len = 1usize;
    segments[0] = (start, end);

    for &(res_start, res_end) in reserved {
        if res_end <= res_start {
            continue;
        }
        let res_start = align_down_u64(res_start, page).max(start);
        let res_end = align_up_u64(res_end, page).min(end);
        if res_end <= res_start {
            continue;
        }
        let mut next = [(0u64, 0u64); MAX_PT_ALLOCATOR_REGIONS];
        let mut next_len = 0usize;
        for &(seg_start, seg_end) in segments.iter().take(seg_len) {
            if res_end <= seg_start || res_start >= seg_end {
                if next_len < MAX_PT_ALLOCATOR_REGIONS {
                    next[next_len] = (seg_start, seg_end);
                    next_len += 1;
                }
                continue;
            }
            if seg_start < res_start && next_len < MAX_PT_ALLOCATOR_REGIONS {
                next[next_len] = (seg_start, res_start);
                next_len += 1;
            }
            if res_end < seg_end && next_len < MAX_PT_ALLOCATOR_REGIONS {
                next[next_len] = (res_end, seg_end);
                next_len += 1;
            }
        }
        segments = next;
        seg_len = next_len;
        if seg_len == 0 {
            break;
        }
    }

    let mut out_len = 0usize;
    for &(seg_start, seg_end) in segments.iter().take(seg_len) {
        if seg_end <= seg_start {
            continue;
        }
        out[out_len] = crate::kernel::frame_allocator::MemoryRegion {
            start: seg_start,
            len: seg_end - seg_start,
            usable: true,
        };
        out_len += 1;
    }
    (out, out_len)
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const fn align_down_u64(value: u64, align: u64) -> u64 {
    value & !(align - 1)
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const fn align_up_u64(value: u64, align: u64) -> u64 {
    value.saturating_add(align - 1) & !(align - 1)
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn setup_bootstrap_mmu() {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M0");
    unsafe {
        let l1_addr = core::ptr::addr_of!(BOOT_L1_TABLE) as u64;
        let l2_addr = core::ptr::addr_of!(BOOT_L2_TABLE) as u64;
        let l1_ptr = core::ptr::addr_of_mut!(BOOT_L1_TABLE.0) as *mut u64;
        let l2_ptr = core::ptr::addr_of_mut!(BOOT_L2_TABLE.0) as *mut u64;

        for idx in 0..512 {
            core::ptr::write(l1_ptr.add(idx), 0);
            core::ptr::write(l2_ptr.add(idx), 0);
        }

        core::ptr::write(
            l1_ptr,
            (l2_addr & !0xFFF) | AARCH64_PTE_VALID | AARCH64_PTE_TABLE,
        );
        for idx in 1..512 {
            let base = (idx as u64) * AARCH64_BLOCK_1G;
            core::ptr::write(
                l1_ptr.add(idx),
                base | AARCH64_PTE_VALID
                    | AARCH64_PTE_AF
                    | AARCH64_PTE_SH_INNER
                    | (AARCH64_ATTRIDX_NORMAL_WB << AARCH64_PTE_ATTR_SHIFT),
            );
        }

        for idx in 0..512 {
            let base = (idx as u64) * AARCH64_BLOCK_2M;
            core::ptr::write(
                l2_ptr.add(idx),
                base | AARCH64_PTE_VALID
                    | AARCH64_PTE_AF
                    | AARCH64_PTE_SH_INNER
                    | (AARCH64_ATTRIDX_NORMAL_WB << AARCH64_PTE_ATTR_SHIFT),
            );
        }

        let device_block = |base: u64| -> u64 {
            base | AARCH64_PTE_VALID
                | AARCH64_PTE_AF
                | (AARCH64_ATTRIDX_DEVICE_NGNRE << AARCH64_PTE_ATTR_SHIFT)
        };
        let gic_l2_index = (0x0800_0000u64 / AARCH64_BLOCK_2M) as usize;
        core::ptr::write(l2_ptr.add(gic_l2_index), device_block(0x0800_0000));
        let uart_l2_index = (AARCH64_UART_MMIO_BASE / AARCH64_BLOCK_2M) as usize;
        core::ptr::write(
            l2_ptr.add(uart_l2_index),
            device_block(AARCH64_UART_MMIO_BASE),
        );

        // AttrIdx0 = normal WB/WA cacheable (0xff).
        // AttrIdx1 = normal WT cacheable (0xbb).
        // AttrIdx2 = normal non-cacheable (0x44).
        // AttrIdx3 = device nGnRE (0x04).
        let mair: u64 = 0xff | (0xbb << 8) | (0x44 << 16) | (0x04 << 24);
        let tcr: u64 = 25u64
            | (1 << 8)
            | (1 << 10)
            | (0b11 << 12)
            | (25u64 << 16)
            | (1u64 << 23)
            | (0b10 << 30)
            | (0b010 << 32);

        core::arch::asm!("msr MAIR_EL1, {0}", in(reg) mair, options(nostack, preserves_flags));
        core::arch::asm!("msr TCR_EL1, {0}", in(reg) tcr, options(nostack, preserves_flags));
        core::arch::asm!("msr TTBR0_EL1, {0}", in(reg) l1_addr, options(nostack, preserves_flags));
        core::arch::asm!("msr TTBR1_EL1, xzr", options(nostack, preserves_flags));
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1");
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1A");
        core::arch::asm!("dsb ish", options(nostack, preserves_flags));
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1B");
        core::arch::asm!("isb", options(nostack, preserves_flags));
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1C");
        core::arch::asm!("tlbi vmalle1", options(nostack, preserves_flags));
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1D");
        // Keep the bootstrap sequence conservative: some emulated CPU configs can
        // fault on IC invalidate-all during early bring-up. We can safely defer this
        // for now because MMU/caches are just being enabled.
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1E");
        core::arch::asm!("dsb ish", options(nostack, preserves_flags));
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1F");
        core::arch::asm!("isb", options(nostack, preserves_flags));
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1G");

        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1H");
        // Preserve implementation-defined/reserved state, and only enable M/C/I.
        // This is safer across CPU models than forcing a fixed bit mask.
        let mut sctlr: u64;
        core::arch::asm!("mrs {0}, SCTLR_EL1", out(reg) sctlr, options(nostack, preserves_flags));
        sctlr |= (1 << 0) | (1 << 2) | (1 << 12);
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1I");
        core::arch::asm!("ic iallu", options(nostack, preserves_flags));
        core::arch::asm!("dsb nsh", options(nostack, preserves_flags));
        core::arch::asm!("isb", options(nostack, preserves_flags));
        core::arch::asm!("msr SCTLR_EL1, {0}", in(reg) sctlr, options(nostack, preserves_flags));
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1J");
        core::arch::asm!("isb", options(nostack, preserves_flags));
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M1K");
    }
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB M2");
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BOOT_MARKER stage=mmu_enabled");
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn dtb_slice_from_start_info(start_info_ptr: usize) -> Option<&'static [u8]> {
    let mut candidate_ptr = start_info_ptr;
    if candidate_ptr == 0 {
        crate::yarm_log!("YARM_AARCH64_DTB_STATUS missing_start_info_ptr");
        candidate_ptr = probe_qemu_virt_dtb_pointer().unwrap_or(0);
        if candidate_ptr != 0 {
            crate::yarm_log!(
                "YARM_AARCH64_DTB_STATUS recovered_ptr ptr=0x{:x}",
                candidate_ptr as u64
            );
        }
    }
    if candidate_ptr == 0 {
        return None;
    }
    let magic_be = unsafe { core::ptr::read_unaligned(candidate_ptr as *const u32) };
    if u32::from_be(magic_be) != 0xd00dfeed {
        crate::yarm_log!(
            "YARM_AARCH64_DTB_STATUS bad_magic value=0x{:x} ptr=0x{:x}",
            u32::from_be(magic_be),
            candidate_ptr as u64
        );
        return None;
    }
    let total_size_be = unsafe { core::ptr::read_unaligned((candidate_ptr + 4) as *const u32) };
    let total_size = u32::from_be(total_size_be) as usize;
    if total_size < 40 || total_size > 2 * 1024 * 1024 {
        crate::yarm_log!(
            "YARM_AARCH64_DTB_STATUS bad_size size={} ptr=0x{:x}",
            total_size,
            candidate_ptr as u64
        );
        return None;
    }
    Some(unsafe { core::slice::from_raw_parts(candidate_ptr as *const u8, total_size) })
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn probe_qemu_virt_dtb_pointer() -> Option<usize> {
    const FDT_MAGIC: u32 = 0xd00dfeed;
    const PROBE_START: u64 = 0x4000_0000;
    // Probe within the first 512 MiB from RAM base. Without firmware-provided
    // RAM size metadata, probing past absent RAM can fault before kernel state
    // is installed (observed at FAR=0x6000_0000 on 512 MiB guests).
    const PROBE_BYTES: u64 = 512 * 1024 * 1024;
    const PROBE_STEP_PAGE: u64 = 0x1000;
    const PROBE_INTRA_PAGE_SCAN_BYTES: u64 = 128;
    const PROBE_INTRA_PAGE_STEP: u64 = 8;

    let candidate_is_valid = |addr: u64| -> bool {
        let raw_magic = unsafe { core::ptr::read_unaligned(addr as *const u32) };
        if u32::from_be(raw_magic) != FDT_MAGIC {
            return false;
        }
        let total_size_be = unsafe { core::ptr::read_unaligned((addr + 4) as *const u32) };
        let total_size = u32::from_be(total_size_be) as usize;
        (40..=2 * 1024 * 1024).contains(&total_size)
    };

    let mut addr = PROBE_START;
    let end = PROBE_START + PROBE_BYTES;
    while addr < end {
        if candidate_is_valid(addr) {
            return Some(addr as usize);
        }
        let page_scan_end = (addr + PROBE_INTRA_PAGE_SCAN_BYTES).min(end);
        let mut probe = addr + PROBE_INTRA_PAGE_STEP;
        while probe < page_scan_end {
            if candidate_is_valid(probe) {
                return Some(probe as usize);
            }
            probe += PROBE_INTRA_PAGE_STEP;
        }
        addr += PROBE_STEP_PAGE;
    }
    None
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
fn encode_irq_desc_gic_cpu_if_base(base: usize, out: &mut [u8]) -> Option<usize> {
    let prefix = b"gic_cpu_if_base=0x";
    if out.len() < prefix.len() + 16 {
        return None;
    }
    out[..prefix.len()].copy_from_slice(prefix);
    let mut cursor = prefix.len();
    let nybbles = core::mem::size_of::<usize>() * 2;
    let mut emitted = false;
    for shift in (0..nybbles).rev() {
        let nibble = ((base >> (shift * 4)) & 0xF) as u8;
        if nibble != 0 || emitted || shift == 0 {
            out[cursor] = if nibble < 10 {
                b'0' + nibble
            } else {
                b'a' + (nibble - 10)
            };
            cursor += 1;
            emitted = true;
        }
    }
    Some(cursor)
}

pub fn emit_panic(info: &core::panic::PanicInfo<'_>) {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    {
        struct PanicSerialWriter;
        impl Write for PanicSerialWriter {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                for line in s.split('\n') {
                    if !line.is_empty() {
                        crate::arch::aarch64::console::write_line(line);
                    }
                }
                Ok(())
            }
        }

        crate::arch::aarch64::console::write_line("YARM_AARCH64_BOOT_MARKER stage=panic");
        let mut writer = PanicSerialWriter;
        let _ = writer.write_str("PANIC ");
        if let Some(location) = info.location() {
            let _ = write!(
                writer,
                "{}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            );
        } else {
            let _ = writer.write_str("<unknown>");
        }
        let _ = writer.write_str(": ");
        let _ = write!(writer, "{}", info.message());
        let _ = writer.write_str("\n");
    }
    #[cfg(any(feature = "hosted-dev", not(target_arch = "aarch64")))]
    let _ = info;
}
