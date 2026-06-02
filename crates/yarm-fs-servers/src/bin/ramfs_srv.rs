// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![cfg_attr(not(feature = "hosted-dev"), no_std)]
#![cfg_attr(not(feature = "hosted-dev"), no_main)]

#[cfg(not(feature = "hosted-dev"))]
yarm_server_runtime::install_freestanding_allocator!(
    1024 * 1024,
    "ramfs server freestanding allocator OOM"
);

#[inline]
fn run() {
    yarm_fs_servers::run_ramfs();
}

#[cfg(feature = "hosted-dev")]
fn main() {
    run();
}

#[cfg(not(feature = "hosted-dev"))]
#[unsafe(no_mangle)]
pub extern "C" fn yarm_user_entry() -> ! {
    yarm_user_rt::user_log!("RAMFS_BIN_ENTRY_START");
    yarm_user_rt::user_log!("RAMFS_BIN_BEFORE_RUN");
    run();
    let ctx = yarm_server_runtime::user_rt::runtime::startup_context();
    if let Some(recv_cap) = ctx.process_manager_service_recv_ep {
        yarm_user_rt::user_log!("RAMFS_SRV_BLOCKING_RECV_LOOP cap={}", recv_cap);
        loop {
            let _ = unsafe { yarm_server_runtime::user_rt::syscall::ipc_recv_v2(recv_cap) };
        }
    }
    yarm_user_rt::user_log!("RAMFS_SRV_NO_RECV_CAP_RESIDENT_YIELD");
    loop {
        let _ = yarm_server_runtime::user_rt::syscall::yield_now();
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
