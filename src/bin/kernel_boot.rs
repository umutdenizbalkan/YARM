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
    const DEBUG_DISPATCH_CONTEXT_LOG: bool = false;
    let cpu = kernel.current_cpu();
    if let Err(err) = yarm::arch::boot_entry::bootstrap_first_user_task(kernel) {
        yarm::pr_err!("failed to bootstrap first user task: {:?}", err);
    }
    yarm::arch::boot_entry::release_secondary_cpus_after_bootstrap();
    // x86_64: unblock the timer ISR from EOI-only mode now that all user tasks
    // are spawned and enqueued. Must come after bootstrap_first_user_task and
    // release_secondary_cpus_after_bootstrap complete.
    yarm::arch::boot_entry::signal_bootstrap_scheduler_ready();
    // x86_64 BT2: arm the BSP LAPIC timer only after bootstrap completes.
    // The timer was intentionally not armed during LAPIC init or
    // run_with_prepared_kernel, so no timer ISR could race with
    // borrow_kernel_for_boot()'s raw &mut alias during ELF loading.
    yarm::arch::boot_entry::start_bsp_periodic_timer(kernel);
    if DEBUG_DISPATCH_CONTEXT_LOG {
        yarm::yarm_log!("BSP_POST_RELEASE cpu={}", cpu.0);
        yarm::yarm_log!("BSP_REDISPATCH_BEGIN cpu={}", cpu.0);
    }
    let observed_cpu = kernel.current_cpu();
    if observed_cpu.0 != yarm::arch::platform_constants::BOOTSTRAP_CPU_ID {
        yarm::yarm_log!(
            "BSP_CPU_IDENTITY_VIOLATION observed_cpu={} expected_cpu=0",
            observed_cpu.0
        );
    }
    assert_eq!(
        observed_cpu.0,
        yarm::arch::platform_constants::BOOTSTRAP_CPU_ID
    );

    let initial = kernel.dispatch_ready_task().ok().flatten();
    if DEBUG_DISPATCH_CONTEXT_LOG {
        yarm::yarm_log!("BSP_REDISPATCH_SELECTED tid={:?}", initial);
        yarm::yarm_log!("YARM_SCHED_LOOP_START dispatched_tid={:?}", initial);
    }
    if let Some(tid) = initial {
        if DEBUG_DISPATCH_CONTEXT_LOG {
            yarm::yarm_log!("BSP_BEFORE_ENTER_USER tid={}", tid);
            yarm::yarm_log!(
                "CTX2 before enter_dispatched_user_task_if_available tid={}",
                tid
            );
            yarm::yarm_log!("DISPATCH: before enter_user_call");
        }
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
