// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]
#![cfg_attr(not(feature = "hosted-dev"), no_main)]

#[cfg(not(test))]
fn run_scheduler_loop(kernel: &mut yarm::kernel::boot::KernelState) {
    if let Err(err) = yarm::arch::boot_entry::bootstrap_first_user_task(kernel) {
        yarm::pr_err!("failed to bootstrap first user task: {:?}", err);
    }

    let initial = kernel.dispatch_ready_task().ok().flatten();
    yarm::yarm_log!("YARM_SCHED_LOOP_START dispatched_tid={:?}", initial);
    yarm::arch::boot_entry::enter_dispatched_user_task_if_available(kernel, initial);
}

fn run() {
    #[cfg(not(test))]
    yarm::arch::boot_entry::run_with_prepared_kernel(run_scheduler_loop);

    #[cfg(test)]
    let _ = yarm::kernel::boot::Bootstrap::init().expect("kernel init");

    #[cfg(not(feature = "hosted-dev"))]
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(feature = "hosted-dev")]
fn main() {
    yarm::arch::boot_entry::run_kernel_boot(run);
}

#[cfg(not(feature = "hosted-dev"))]
#[unsafe(no_mangle)]
pub extern "C" fn yarm_kernel_main(start_info_ptr: usize) -> ! {
    yarm::arch::boot_entry::prepare_arch_boot(start_info_ptr);
    yarm::arch::boot_entry::run_kernel_boot(run);
    unreachable!("kernel run loop should not return");
}

#[cfg(not(feature = "hosted-dev"))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    yarm::arch::boot_entry::emit_panic(info);
    loop {}
}
