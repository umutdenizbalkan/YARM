// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![cfg_attr(not(feature = "hosted-dev"), no_std)]
#![cfg_attr(not(feature = "hosted-dev"), no_main)]

#[cfg(not(feature = "hosted-dev"))]
yarm::install_freestanding_allocator!(
    2 * 1024 * 1024,
    "control-plane server freestanding allocator OOM"
);

#[inline]
fn run() {
    yarm_control_plane_servers::run_init_server();
}

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
    ];
    if startup_slots_ptr != 0 && startup_slots_len >= slots.len() {
        // SAFETY: kernel provides user entry args; when pointer/len is valid for
        // the startup block contract we copy exactly 11 u64 entries.
        let src = startup_slots_ptr as *const u64;
        let mut index = 0usize;
        while index < slots.len() {
            // SAFETY: bounded by `slots.len()` and guarded by non-zero pointer
            // + contract length check above.
            slots[index] = unsafe { core::ptr::read(src.add(index)) };
            index += 1;
        }
    }
    yarm::install_startup_arg_slots(slots);
    run();
    loop {}
}

#[cfg(not(feature = "hosted-dev"))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
