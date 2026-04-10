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
    mov x0, x20
    .weak yarm_kernel_main
    bl yarm_kernel_main
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
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BOOT_MARKER stage=run_with_prepared_kernel");
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
    crate::arch::aarch64::console::write_line("YARM_AARCH64_BOOT_MARKER stage=prepare_arch_boot");
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
