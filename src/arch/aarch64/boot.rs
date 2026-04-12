// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
use core::arch::global_asm;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
use core::fmt::Write;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
global_asm!(
    r#"
    .section .bss.bootstack,"aw",@nobits
    .align 16
boot_stack_aarch64:
    .skip 65536
boot_stack_aarch64_end:
    .align 16
exc_stack_aarch64:
    .skip 8192
exc_stack_aarch64_end:

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
const AARCH64_ATTRIDX_DEVICE_NGNRE: u64 = 1;
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
    mov x5, sp
    adrp x6, exc_stack_aarch64_end
    add x6, x6, :lo12:exc_stack_aarch64_end
    and x6, x6, #~0xf
    mov sp, x6
    stp x5, x30, [sp, #-16]!
    mrs x1, esr_el1
    mrs x2, far_el1
    mrs x3, elr_el1
    mrs x4, spsr_el1
    msr daifset, #0xf
    bl yarm_aarch64_vector_entry
    ldp x5, x30, [sp], #16
    mov sp, x5
1:
    wfe
    b 1b
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
#[unsafe(no_mangle)]
extern "C" fn yarm_aarch64_vector_entry(kind: u64, esr_el1: u64, far_el1: u64, elr_el1: u64, spsr_el1: u64) {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_VECTOR_ENTRY");
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BOOT_MARKER stage=exception");
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
        esr_el1,
        far_el1,
        elr_el1,
        spsr_el1
    );
    let line_len = writer.len;
    if let Ok(msg) = core::str::from_utf8(&line[..line_len]) {
        crate::arch::aarch64::console::write_line(msg);
    }
    match kind {
        1 => crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND sync_current_sp0"),
        2 => crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND irq_current_sp0"),
        3 => crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND fiq_current_sp0"),
        4 => crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND serr_current_sp0"),
        5 => crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND sync_current_spx"),
        6 => crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND irq_current_spx"),
        7 => crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND fiq_current_spx"),
        8 => crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND serr_current_spx"),
        9 => crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND sync_lower_a64"),
        10 => crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND irq_lower_a64"),
        11 => crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND fiq_lower_a64"),
        12 => crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND serr_lower_a64"),
        13 => crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND sync_lower_a32"),
        14 => crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND irq_lower_a32"),
        15 => crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND fiq_lower_a32"),
        16 => crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND serr_lower_a32"),
        _ => crate::arch::aarch64::console::write_line("YARM_AARCH64_EXCEPTION_KIND unknown"),
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const RING3_INIT_SERVER_TID: u64 = 1;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const RING3_INIT_SERVER_ENTRY: u64 = 0x0040_1000;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
const RING3_INIT_SERVER_CODE_PAGE: u64 = 0x0040_0000;

#[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
pub fn bootstrap_first_user_task(
    kernel: &mut crate::kernel::boot::KernelState,
) -> Result<(), crate::kernel::boot::KernelError> {
    use crate::services::control_plane::init::service::{
        InitRuntimeBootConfig, run_minimum_profile_with_kernel,
    };

    crate::yarm_log!("YARM_INIT_START arch=aarch64 mode=initramfs_min_profile");
    if let Ok(summary) = run_minimum_profile_with_kernel(kernel, InitRuntimeBootConfig::baseline())
    {
        crate::yarm_log!(
            "YARM_INIT_DONE arch=aarch64 phase={:?} seeded={} initramfs_handled={} devfs_handled={}",
            summary.init_phase,
            summary.seeded_registrations,
            summary.initramfs_handled,
            summary.devfs_handled
        );
        return Ok(());
    }

    use crate::kernel::boot::UserImageSpec;
    use crate::kernel::task::TaskClass;
    use crate::kernel::vm::{PageFlags, VirtAddr};

    if kernel.task_asid(RING3_INIT_SERVER_TID).is_some() {
        return Ok(());
    }

    let (asid, aspace_cap) = kernel.create_user_address_space()?;
    kernel.spawn_user_task_from_image(UserImageSpec {
        tid: RING3_INIT_SERVER_TID,
        entry: RING3_INIT_SERVER_ENTRY as usize,
        asid: Some(asid),
        class: TaskClass::SystemServer,
    })?;

    let (_mem_id, mem_cap) = kernel.alloc_anonymous_memory_object()?;
    kernel.map_user_page_with_caps(
        aspace_cap,
        mem_cap,
        VirtAddr(RING3_INIT_SERVER_CODE_PAGE),
        PageFlags::USER_RW,
    )?;

    // movz x8,#0 ; svc #0 ; b .
    let code: [u8; 12] = [
        0x08, 0x00, 0x80, 0xD2, 0x01, 0x00, 0x00, 0xD4, 0x00, 0x00, 0x00, 0x14,
    ];
    kernel.write_user_memory(
        RING3_INIT_SERVER_TID,
        RING3_INIT_SERVER_ENTRY as usize,
        &code,
    )?;
    let _ = kernel.protect_user_page(
        aspace_cap,
        VirtAddr(RING3_INIT_SERVER_CODE_PAGE),
        PageFlags::USER_RX,
    )?;
    crate::yarm_log!(
        "YARM_INIT_DONE arch=aarch64 phase=fallback_ring3_stub seeded=0 initramfs_handled=0 devfs_handled=0"
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
    let mut kernel = crate::kernel::boot::Bootstrap::init().expect("kernel init");
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    unsafe {
        core::arch::asm!(
            "msr ttbr0_el1, {0}",
            "isb",
            in(reg) saved_ttbr0,
            options(nostack, preserves_flags)
        );
    }
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    {
        // Do not touch EL1 physical timer registers during early bootstrap.
        // On some boot paths (e.g. direct EL1 entry), CNTHCTL_EL2 may still
        // trap EL1 timer sysreg accesses, which raises a synchronous exception
        // right after this stage marker. Timer/IRQ bring-up is deferred to the
        // normal init path once platform control is fully established.
    }
    crate::yarm_log!(
        "YARM_BOOT_OK present_cpus={} present_bitmap=0x{:x} online_cpus={}",
        kernel.present_cpu_count(),
        kernel.present_cpu_bitmap(),
        kernel.online_cpu_count()
    );
    run(&mut kernel);
}

pub fn prepare_arch_boot(_start_info_ptr: usize) {
    #[cfg(all(not(feature = "hosted-dev"), target_arch = "aarch64"))]
    {
        let start_info_ptr = _start_info_ptr;
        crate::arch::aarch64::console::write_line("YARM_AARCH64_BREADCRUMB P0");
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
                if let (Some(start), Some(len)) = (parsed.memory_start, parsed.memory_len) {
                    let _ = crate::kernel::frame_allocator::init_pt_frame_allocator(&[
                        crate::kernel::frame_allocator::MemoryRegion {
                            start,
                            len,
                            usable: true,
                        },
                    ]);
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
        core::ptr::write(
            l1_ptr.add(1),
            AARCH64_BLOCK_1G
                | AARCH64_PTE_VALID
                | AARCH64_PTE_AF
                | AARCH64_PTE_SH_INNER
                | (AARCH64_ATTRIDX_NORMAL_WB << AARCH64_PTE_ATTR_SHIFT),
        );

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
        core::ptr::write(l2_ptr.add(uart_l2_index), device_block(AARCH64_UART_MMIO_BASE));

        let mair: u64 = 0x04_ff;
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
    if start_info_ptr == 0 {
        return None;
    }
    let total_size_be = unsafe { core::ptr::read_unaligned((start_info_ptr + 4) as *const u32) };
    let total_size = u32::from_be(total_size_be) as usize;
    if total_size < 40 || total_size > 2 * 1024 * 1024 {
        return None;
    }
    Some(unsafe { core::slice::from_raw_parts(start_info_ptr as *const u8, total_size) })
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
