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
    startup_slots_ptr: usize,
    startup_slots_len: usize,
    _startup_slots_reserved: usize,
) -> ! {
    // Startup ABI slot contract:
    //   0 => task_id / tid
    //   1 => process-manager request send cap
    //   2 => process-manager reply recv cap
    let mut slots = [
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
        0,
        0,
    ];
    if startup_slots_ptr != 0 && startup_slots_len >= slots.len() {
        // SAFETY: kernel contract provides startup block pointer + count.
        let src = startup_slots_ptr as *const u64;
        let mut index = 0usize;
        while index < slots.len() {
            // SAFETY: bounded by `slots.len()` and guarded by pointer/len check.
            slots[index] = unsafe { core::ptr::read(src.add(index)) };
            index += 1;
        }
    }
    yarm_server_runtime::install_startup_arg_slots(slots);
    run();
    loop {}
}

#[cfg(not(feature = "hosted-dev"))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
