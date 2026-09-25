// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![cfg_attr(not(feature = "hosted-dev"), no_std)]
#![cfg_attr(not(feature = "hosted-dev"), no_main)]

#[cfg(not(feature = "hosted-dev"))]
yarm::install_freestanding_allocator!(256 * 1024, "supervisor freestanding allocator OOM");

#[inline]
fn run() {
    yarm_control_plane_servers::run_supervisor_server();
}

#[cfg(feature = "hosted-dev")]
fn main() {
    run();
}

#[cfg(not(feature = "hosted-dev"))]
#[unsafe(no_mangle)]
pub extern "C" fn yarm_user_entry() -> ! {
    yarm_user_rt::user_log!("SUP_BIN_ENTRY_START");
    // U9-PAGEFAULT1 §3 — the DEMAND page-fault witness.
    //
    // IT RUNS HERE, NOT IN INIT, and the reason is measured rather than chosen. The first live
    // run placed it in the init server and the fault fired exactly as designed — a user write to
    // a non-present page inside the grown brk window, error 0x6 — but the recovery allocated its
    // frame and then failed: `VM_FULL reason=mapping_bookkeeping_full asid=Some(1)
    // max_mappings=128`. Init's address space is AT its mapping ceiling on the provisioned
    // profiles, which is the same pressure `ipc-send-final-fault-witness` and
    // `timer5-idle-return-witness` each measured and documented in their own feature comments.
    //
    // Raising `MAX_MAPPINGS` is forbidden — a witness must not need a capacity increase to pass —
    // so the witness moves to a task that has headroom instead. The supervisor is that task:
    // measured on the same boot it performs roughly a third of init's mapping work.
    //
    // It runs FIRST, before any service work, so its markers cannot interleave with the
    // supervisor's own and so a page it demands cannot be mistaken for one a service needed. It
    // grows only THIS task's brk window and touches only inside it.
    #[cfg(feature = "pagefault1-demand-witness")]
    yarm_user_rt::pagefault1_demand_witness::run_once();
    // QEMU-BASELINE1 §3 — the timer contract's controlled workload. Also FIRST, for the same
    // reason: nothing of the supervisor's own is in flight while it spins, so every tick it is
    // interrupted by is attributable to it alone.
    #[cfg(feature = "timer-contract-witness")]
    yarm_user_rt::timer_contract_witness::run_once();
    yarm_user_rt::user_log!("SUP_BEFORE_RUN");
    run();
    let ctx = yarm_user_rt::runtime::startup_context();
    if let Some(recv_cap) = ctx.supervisor_control_recv_ep {
        yarm_user_rt::user_log!("SUP_BLOCKING_RECV_LOOP cap={}", recv_cap);
        loop {
            let _ = unsafe { yarm::user_rt::syscall::ipc_recv_v2(recv_cap) };
        }
    }
    yarm_user_rt::user_log!("SUP_NO_RECV_CAP");
    loop {
        let _ = yarm::user_rt::syscall::yield_now();
    }
}

#[cfg(not(feature = "hosted-dev"))]
#[unsafe(no_mangle)]
pub extern "C" fn _start(
    startup_task_id: u64,
    startup_proc_mgr_request_send_cap: u64,
    startup_proc_mgr_reply_recv_cap: u64,
    startup_slots_ptr: usize,
    startup_slots_len: usize,
    _startup_slots_reserved: usize,
) -> ! {
    yarm::user_rt::runtime::enter_user_entrypoint(
        startup_task_id,
        startup_proc_mgr_request_send_cap,
        startup_proc_mgr_reply_recv_cap,
        startup_slots_ptr,
        startup_slots_len,
        yarm_user_entry,
    )
}

#[cfg(not(feature = "hosted-dev"))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
