// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![cfg_attr(not(feature = "hosted-dev"), no_std)]
#![cfg_attr(not(feature = "hosted-dev"), no_main)]

// PM sequentially reads each ELF binary via VFS into a Vec<u8> before spawning.
// PM heap target is 256 KiB. Runtime ELF staging Vecs are reclaimed between spawns by the free-list allocator.
#[cfg(not(feature = "hosted-dev"))]
yarm::install_freestanding_allocator!(256 * 1024, "process manager freestanding allocator OOM");

#[inline]
fn run() {
    yarm_control_plane_servers::run_process_manager();
}

#[cfg(feature = "hosted-dev")]
fn main() {
    run();
}

#[cfg(not(feature = "hosted-dev"))]
#[unsafe(no_mangle)]
pub extern "C" fn yarm_user_entry() -> ! {
    yarm_user_rt::user_log!("PM_BIN_ENTRY_START");
    yarm_user_rt::user_log!("PM_BEFORE_RUN");
    run();
    let ctx = yarm_user_rt::runtime::startup_context();
    if let Some(recv_cap) = ctx.pm_request_recv_cap {
        yarm_user_rt::user_log!("PM_BLOCKING_RECV_LOOP cap={}", recv_cap);
        loop {
            let _ = unsafe { yarm::user_rt::syscall::ipc_recv_v2(recv_cap) };
        }
    }
    yarm_user_rt::user_log!("PM_NO_RECV_CAP");
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
    // Best-effort diagnostic: log before entering the halt loop so OOM and other
    // panics are visible in the QEMU trace.  user_log is a simple syscall and is
    // safe to call even during allocation failures.
    yarm_user_rt::user_log!("PM_PANIC");
    loop {}
}
