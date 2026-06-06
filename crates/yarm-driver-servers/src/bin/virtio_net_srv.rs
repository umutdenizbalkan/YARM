// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![cfg_attr(not(feature = "hosted-dev"), no_std)]
#![cfg_attr(not(feature = "hosted-dev"), no_main)]

#[cfg(not(feature = "hosted-dev"))]
yarm_server_runtime::install_freestanding_allocator!(
    256 * 1024,
    "virtio_net server freestanding allocator OOM"
);

#[inline]
fn run() -> ! {
    yarm_user_rt::user_log!("VIRTIO_NET_BIN_ENTRY_START");
    yarm_driver_servers::run_virtio_net();
    unreachable!("virtio_net service loop must remain resident")
}

#[cfg(feature = "hosted-dev")]
fn main() {
    run();
}

#[cfg(not(feature = "hosted-dev"))]
#[unsafe(no_mangle)]
pub extern "C" fn yarm_user_entry() -> ! {
    run()
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
    yarm_server_runtime::user_rt::runtime::enter_user_entrypoint(
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
