// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]
#![cfg_attr(not(feature = "hosted-dev"), no_main)]

#[cfg(not(feature = "hosted-dev"))]
#[global_allocator]
static KERNEL_GLOBAL_ALLOCATOR: yarm::kernel::global_allocator::KernelGlobalAllocator =
    yarm::kernel::global_allocator::KERNEL_GLOBAL_ALLOCATOR;

#[cfg(not(test))]
fn run_scheduler_loop(kernel: &mut yarm::kernel::boot::KernelState) {
    let cpu = kernel.current_cpu();
    if let Err(err) = yarm::arch::boot_entry::bootstrap_first_user_task(kernel) {
        yarm::pr_err!("failed to bootstrap first user task: {:?}", err);
    }
    yarm::arch::boot_entry::release_secondary_cpus_after_bootstrap();
    yarm::yarm_log!("BSP_POST_RELEASE cpu={}", cpu.0);
    yarm::yarm_log!("BSP_REDISPATCH_BEGIN cpu={}", cpu.0);

    let initial = kernel.dispatch_ready_task().ok().flatten();
    yarm::yarm_log!("BSP_REDISPATCH_SELECTED tid={:?}", initial);
    yarm::yarm_log!("YARM_SCHED_LOOP_START dispatched_tid={:?}", initial);
    if let Some(tid) = initial {
        yarm::yarm_log!("BSP_BEFORE_ENTER_USER tid={}", tid);
        yarm::yarm_log!("DISPATCH: before enter_user_call");
        yarm::arch::boot_entry::enter_dispatched_user_task_if_available(kernel, Some(tid));
    } else {
        if cpu.0 == yarm::arch::platform_constants::BOOTSTRAP_CPU_ID {
            yarm::yarm_log!("BSP_REDISPATCH_SELECTED tid=None");
        } else {
            yarm::yarm_log!("AP_IDLE_PATH cpu={} dispatched_tid=None", cpu.0);
        }
    }
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
    #[cfg(target_arch = "x86_64")]
    run();
    #[cfg(not(target_arch = "x86_64"))]
    yarm::arch::boot_entry::run_kernel_boot(run);
    unreachable!("kernel run loop should not return");
}

#[cfg(not(feature = "hosted-dev"))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    yarm::arch::boot_entry::emit_panic(info);
    loop {}
}
