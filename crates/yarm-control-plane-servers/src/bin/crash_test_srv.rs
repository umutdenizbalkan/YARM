// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Crash-test service binary (SUP-L5B).
//!
//! Entry glue only. The service body lives in
//! `yarm_control_plane_servers::control_plane::crash_test`, reached through
//! `run_crash_test_srv()`, matching every other control-plane bin. A service binary may
//! carry runtime/entry glue but must delegate its behaviour to the service/runtime
//! entrypoint — `scripts/check-service-arch-boundary.sh` enforces that.

#![cfg_attr(not(feature = "hosted-dev"), no_std)]
#![cfg_attr(not(feature = "hosted-dev"), no_main)]

#[cfg(not(feature = "hosted-dev"))]
yarm::install_freestanding_allocator!(64 * 1024, "crash_test_srv allocator OOM");

#[cfg(feature = "hosted-dev")]
fn main() {
    // Hosted builds cannot take the deterministic userspace fault the freestanding body
    // ends in, so they report the markers and exit instead. The delay bound is shared with
    // the service module so the two cannot drift.
    eprintln!("CRASH_TEST_SRV_ENTRY");
    eprintln!("CRASH_TEST_SRV_READY");
    eprintln!("CRASH_TEST_SRV_DELAY_BEGIN");
    for _ in 0..yarm_control_plane_servers::control_plane::crash_test::CRASH_TEST_DELAY_YIELDS {
        core::hint::spin_loop();
    }
    eprintln!("CRASH_TEST_SRV_DELAY_DONE");
    eprintln!("CRASH_TEST_SRV_EXIT_NOW");
}

#[cfg(not(feature = "hosted-dev"))]
#[unsafe(no_mangle)]
pub extern "C" fn yarm_user_entry() -> ! {
    yarm_control_plane_servers::run_crash_test_srv()
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
