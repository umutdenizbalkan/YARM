// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![cfg_attr(not(feature = "hosted-dev"), no_std)]
#![cfg_attr(not(feature = "hosted-dev"), no_main)]

#[cfg(not(feature = "hosted-dev"))]
yarm::install_freestanding_allocator!(64 * 1024, "crash_test_srv allocator OOM");

const CRASH_TEST_DELAY_YIELDS: usize = 128;

#[inline]
fn emit_start_markers() {
    #[cfg(not(feature = "hosted-dev"))]
    {
        let ctx = yarm_user_rt::runtime::startup_context();
        if ctx.task_id != 0 {
            yarm_user_rt::user_log!("CRASH_TEST_SRV_ENTRY tid={}", ctx.task_id);
        } else {
            yarm_user_rt::user_log!("CRASH_TEST_SRV_ENTRY");
        }
    }
    #[cfg(feature = "hosted-dev")]
    {
        eprintln!("CRASH_TEST_SRV_ENTRY");
    }

    #[cfg(not(feature = "hosted-dev"))]
    yarm_user_rt::user_log!("CRASH_TEST_SRV_READY");
    #[cfg(feature = "hosted-dev")]
    eprintln!("CRASH_TEST_SRV_READY");
}

#[inline]
fn wait_for_test_delay() {
    #[cfg(not(feature = "hosted-dev"))]
    yarm_user_rt::user_log!("CRASH_TEST_SRV_DELAY_BEGIN");
    #[cfg(feature = "hosted-dev")]
    eprintln!("CRASH_TEST_SRV_DELAY_BEGIN");
    for _ in 0..CRASH_TEST_DELAY_YIELDS {
        #[cfg(not(feature = "hosted-dev"))]
        {
            let _ = yarm::user_rt::syscall::yield_now();
        }
        #[cfg(feature = "hosted-dev")]
        core::hint::spin_loop();
    }
    #[cfg(not(feature = "hosted-dev"))]
    yarm_user_rt::user_log!("CRASH_TEST_SRV_DELAY_DONE");
    #[cfg(feature = "hosted-dev")]
    eprintln!("CRASH_TEST_SRV_DELAY_DONE");
}

#[cfg(feature = "hosted-dev")]
fn main() {
    emit_start_markers();
    wait_for_test_delay();
    eprintln!("CRASH_TEST_SRV_EXIT_NOW");
}

#[cfg(not(feature = "hosted-dev"))]
#[unsafe(no_mangle)]
pub extern "C" fn yarm_user_entry() -> ! {
    emit_start_markers();
    wait_for_test_delay();
    yarm_user_rt::user_log!("CRASH_TEST_SRV_FAULT_NOW");
    // SUP-L5B intentionally uses a deterministic userspace fault rather than
    // adding a new exit/config ABI. SUP-L6 will prove the restart-count loop.
    unsafe {
        core::ptr::write_volatile(core::ptr::null_mut::<u64>(), 0x4352_4153_485f_5453);
    }
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
