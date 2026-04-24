// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]
#![cfg_attr(not(feature = "hosted-dev"), no_main)]

#[inline]
#[cfg(not(test))]
fn run() {
    yarm_server_runtime::run_posix_compat_server();
}

#[inline]
#[cfg(test)]
fn run() {}

#[cfg(feature = "hosted-dev")]
fn main() {
    run();
}

#[cfg(not(feature = "hosted-dev"))]
#[unsafe(no_mangle)]
pub extern "C" fn _start(
    startup_task_id: u64,
    startup_proc_mgr_request_send_cap: u64,
    startup_proc_mgr_reply_recv_cap: u64,
) -> ! {
    // Startup ABI slot contract:
    //   0 => task_id / tid
    //   1 => process-manager request send cap
    //   2 => process-manager reply recv cap
    yarm_server_runtime::install_startup_arg_slots([
        startup_task_id,
        startup_proc_mgr_request_send_cap,
        startup_proc_mgr_reply_recv_cap,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]);
    run();
    loop {}
}

#[cfg(not(feature = "hosted-dev"))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
