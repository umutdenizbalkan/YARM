// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Crash-test service body (SUP-L5B).
//!
//! This is the service behind `src/bin/crash_test_srv.rs`. It lives in the library rather
//! than the bin for the same reason every other control-plane service does: a service
//! binary may carry runtime/entry glue only and must delegate its behaviour to the
//! service/runtime entrypoint. `scripts/check-service-arch-boundary.sh` enforces that by
//! requiring each `*_srv.rs` bin to call through
//! `yarm_<domain>_servers::…run_/main_/enter_user_entrypoint`.
//!
//! The behaviour is unchanged from the version that lived in the bin: emit the entry and
//! ready markers, yield a fixed number of times so the supervisor observes a healthy task
//! first, then take a deterministic userspace fault. SUP-L5B deliberately uses a fault
//! rather than adding a new exit/config ABI; SUP-L6 proves the restart-count loop.

/// Number of yields between `CRASH_TEST_SRV_READY` and the fault, so the supervisor sees a
/// running task before it dies.
pub const CRASH_TEST_DELAY_YIELDS: usize = 128;

/// Emit the entry + ready markers.
#[inline]
pub fn emit_start_markers() {
    #[cfg(not(feature = "hosted-dev"))]
    {
        let ctx = yarm_user_rt::runtime::startup_context();
        if ctx.task_id != 0 {
            yarm_user_rt::user_log!("CRASH_TEST_SRV_ENTRY tid={}", ctx.task_id);
        } else {
            yarm_user_rt::user_log!("CRASH_TEST_SRV_ENTRY");
        }
        yarm_user_rt::user_log!("CRASH_TEST_SRV_READY");
    }
}

/// Yield [`CRASH_TEST_DELAY_YIELDS`] times, bracketed by the delay markers.
#[inline]
pub fn wait_for_test_delay() {
    #[cfg(not(feature = "hosted-dev"))]
    {
        yarm_user_rt::user_log!("CRASH_TEST_SRV_DELAY_BEGIN");
        for _ in 0..CRASH_TEST_DELAY_YIELDS {
            let _ = yarm_user_rt::syscall::yield_now();
        }
        yarm_user_rt::user_log!("CRASH_TEST_SRV_DELAY_DONE");
    }
}

/// The freestanding service body: markers, delay, then the deterministic fault.
///
/// Never returns — the null write faults, and the loop after it exists only so the
/// signature is honest if a future architecture were to make that write recoverable.
#[cfg(not(feature = "hosted-dev"))]
pub fn run() -> ! {
    emit_start_markers();
    wait_for_test_delay();
    yarm_user_rt::user_log!("CRASH_TEST_SRV_FAULT_NOW");
    // SUP-L5B intentionally uses a deterministic userspace fault rather than
    // adding a new exit/config ABI. SUP-L6 will prove the restart-count loop.
    unsafe {
        core::ptr::write_volatile(core::ptr::null_mut::<u64>(), 0x4352_4153_485f_5453);
    }
    loop {
        let _ = yarm_user_rt::syscall::yield_now();
    }
}
