// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![cfg_attr(not(feature = "hosted-dev"), no_std)]
#![cfg_attr(not(feature = "hosted-dev"), no_main)]

#[cfg(not(feature = "hosted-dev"))]
yarm::install_freestanding_allocator!(
    256 * 1024,
    "driver manager freestanding allocator OOM"
);

#[inline]
fn run() {
    yarm_control_plane_servers::run_driver_manager();
}

#[cfg(feature = "hosted-dev")]
fn main() {
    run();
}

#[cfg(not(feature = "hosted-dev"))]
#[unsafe(no_mangle)]
pub extern "C" fn yarm_user_entry() -> ! {
    yarm_user_rt::user_log!("DRIVER_MANAGER_BIN_ENTRY_START");
    yarm_user_rt::user_log!("DRIVER_MANAGER_BEFORE_RUN");
    run();
    let ctx = yarm_user_rt::runtime::startup_context();
    if let Some(recv_cap) = ctx.process_manager_service_recv_ep {
        yarm_user_rt::user_log!("DRIVER_MANAGER_BLOCKING_RECV_LOOP cap={}", recv_cap);
        loop {
            let _ = unsafe { yarm::user_rt::syscall::ipc_recv_v2(recv_cap) };
        }
    }
    yarm_user_rt::user_log!("DRIVER_MANAGER_NO_RECV_CAP");
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
