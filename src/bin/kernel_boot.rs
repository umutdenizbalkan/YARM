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
    // U9-RECV-BLOCK2 §3 — the one-shot x86_64 SMP-unlock audit, driven from the BOOT OWNERSHIP
    // POINT that already holds `&mut KernelState`, so it costs no broad acquisition.
    //
    // The audit is what clears an AP's wake-only bit and drives `live_ap_user_dispatch` ->
    // `build_ap_workload`. Its two historical call sites are inside `KernelState::handle_trap`'s
    // broad syscall and timer arms, which made it depend on some syscall class always falling
    // through to the broad dispatcher — an incidental dependency, and one U9-RECV-BLOCK1 broke by
    // closing the receive family. U9-RECV-BLOCK1 §6 repaired it from the trap path at the cost of
    // a `with_cpu`; this drives the same one-shot body from here instead, and the acquisition is
    // gone.
    //
    // Every gate the audit applies holds AT THIS POINT, which is why this is the right owner and
    // `bootstrap_first_user_task` is not:
    //
    //   * `present > 1` — `release_secondary_cpus_after_bootstrap()` ran above, so the APs this
    //     audit admits are online.
    //   * `unlock_graduated_proof_completed()` — set by the graduated proof, which
    //     `bootstrap_first_user_task` ran above. The ordering the audit's own comment requires
    //     (graduated evidence first, with `online == 1`) is therefore satisfied, not bypassed.
    //   * a real user task current (`tid != 0`) — `dispatch_ready_task()` has just made one
    //     current. This is the gate that keeps the audit off `bootstrap_first_user_task`, and it
    //     is the same reason `run_cross_arch_live_audit_at_first_dispatch` sits at first dispatch
    //     rather than at bootstrap.
    //
    // The audit body, its one-shot latch and its provisioning policy are untouched; only the
    // driver moved, and the historical trap-path hooks beside it are left exactly as they are.
    if initial.is_some() {
        // U9-TIMER3 §2 — the five one-shot diagnostic proofs, driven from the same ownership point
        // and for the same reason the SMP-unlock audit is: `dispatch_ready_task()` has just made a
        // real user task current, which is the ONE prerequisite all five share and the only thing
        // their historical timer callsite was providing. Before this line no real user task is
        // current, which is measurable — every proof returns silently there.
        //
        // They run BEFORE the SMP-unlock audit, which is the order the broad timer arm called them
        // in and which is load-bearing: `smp_ready` reports `online_cpu_count()`, and the audit
        // below is what admits an AP to the scheduler.
        kernel.run_one_shot_diagnostic_proofs_at_first_dispatch();
        kernel.maybe_run_x86_smp_unlock_audit();
    }
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
    // The per-architecture choice between calling `run` directly and going through
    // `run_kernel_boot`'s staging pass lives in `arch::boot_entry`, not here: this bin must
    // route ISA details through `src/arch/*`.
    yarm::arch::boot_entry::enter_kernel_run_loop(run);
    unreachable!("kernel run loop should not return");
}

#[cfg(not(feature = "hosted-dev"))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    yarm::arch::boot_entry::emit_panic(info);
    loop {}
}
