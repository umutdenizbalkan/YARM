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
    .skip 16384
boot_stack_aarch64_end:

    .section .text.boot,"ax",@progbits
    .weak _start
    .type _start,%function
_start:
    mov x20, x0
    adrp x0, boot_stack_aarch64_end
    add x0, x0, :lo12:boot_stack_aarch64_end
    mov sp, x0
    bl yarm_aarch64_boot_marker_start
    bl yarm_aarch64_enter_el1_if_needed
    bl yarm_aarch64_enable_fp_simd
    mov x0, x20
    .weak yarm_kernel_main
    bl yarm_kernel_main
1:
    wfe
    b 1b

    .global yarm_aarch64_enter_el1_if_needed
    .type yarm_aarch64_enter_el1_if_needed,%function
yarm_aarch64_enter_el1_if_needed:
    mrs x0, CurrentEL
    lsr x0, x0, #2
    cmp x0, #0x2
    b.ne 2f
    mrs x1, HCR_EL2
    orr x1, x1, #(1 << 31)
    msr HCR_EL2, x1
    mov x1, #(3 << 20)
    msr CPACR_EL1, x1
    mrs x1, SCTLR_EL1
    msr SCTLR_EL1, x1
    msr SP_EL1, sp
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
    .align 11
    .global yarm_aarch64_vector_table_el1
    .type yarm_aarch64_vector_table_el1,%function
yarm_aarch64_vector_table_el1:
    b yarm_aarch64_vector_sync_current
    .space 124
    b yarm_aarch64_vector_irq_current
    .space 124
    b yarm_aarch64_vector_fiq_current
    .space 124
    b yarm_aarch64_vector_serror_current
    .space 124
    b yarm_aarch64_vector_sync_lower_a64
    .space 124
    b yarm_aarch64_vector_irq_lower_a64
    .space 124
    b yarm_aarch64_vector_fiq_lower_a64
    .space 124
    b yarm_aarch64_vector_serror_lower_a64
    .space 124
    b yarm_aarch64_vector_sync_lower_a32
    .space 124
    b yarm_aarch64_vector_irq_lower_a32
    .space 124
    b yarm_aarch64_vector_fiq_lower_a32
    .space 124
    b yarm_aarch64_vector_serror_lower_a32
    .space 124
    b yarm_aarch64_vector_sync_lower_a64_sp0
    .space 124
    b yarm_aarch64_vector_irq_lower_a64_sp0
    .space 124
    b yarm_aarch64_vector_fiq_lower_a64_sp0
    .space 124
    b yarm_aarch64_vector_serror_lower_a64_sp0
    .space 124

    .macro YARM_AARCH64_VECTOR_STUB name kind
    .global \name
    .type \name,%function
\name:
    mov x0, #\kind
    b yarm_aarch64_vector_dispatch
    .endm

    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_sync_current, 1
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_irq_current, 2
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_fiq_current, 3
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_serror_current, 4
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_sync_lower_a64, 5
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_irq_lower_a64, 6
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_fiq_lower_a64, 7
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_serror_lower_a64, 8
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_sync_lower_a32, 9
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_irq_lower_a32, 10
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_fiq_lower_a32, 11
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_serror_lower_a32, 12
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_sync_lower_a64_sp0, 13
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_irq_lower_a64_sp0, 14
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_fiq_lower_a64_sp0, 15
    YARM_AARCH64_VECTOR_STUB yarm_aarch64_vector_serror_lower_a64_sp0, 16

    .global yarm_aarch64_vector_dispatch
    .type yarm_aarch64_vector_dispatch,%function
yarm_aarch64_vector_dispatch:
    mrs x1, esr_el1
    mrs x2, far_el1
    bl yarm_aarch64_vector_entry
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
extern "C" fn yarm_aarch64_vector_entry(kind: u64, esr_el1: u64, far_el1: u64) {
    crate::arch::aarch64::console::write_line("YARM_AARCH64_VECTOR_ENTRY");
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BOOT_MARKER stage=exception");
    let _ = (kind, esr_el1, far_el1);
}

pub fn bootstrap_first_user_task(
    _kernel: &mut crate::kernel::boot::KernelState,
) -> Result<(), crate::kernel::boot::KernelError> {
    Ok(())
}

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
    let mut kernel = crate::kernel::boot::Bootstrap::init().expect("kernel init");
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
                        crate::kernel::frame_allocator::MemoryRegion { start, len },
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
    }
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
