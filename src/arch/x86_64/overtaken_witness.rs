// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP3-ACCEPTANCE §2 — the x86_64 programs of the overtaken-deferral witness
//! (`crate::kernel::overtaken_witness`). Built only with `x86-overtaken-witness` and armed only by
//! `yarm.x86_64_overtaken_witness=1` on the SMP oracle's reply profile, whose two tasks — the
//! client on CPU 0 and the server on CPU 1 — run these programs instead of its own
//! (`KernelState::provision_overtaken_witness`). Everything they do is a real syscall.

use core::sync::atomic::{AtomicBool, Ordering};

core::arch::global_asm!(include_str!("overtaken_witness.S"));

unsafe extern "C" {
    static yarm_ovt_x86_w_start: u8;
    static yarm_ovt_x86_w_end: u8;
    static yarm_ovt_x86_k_start: u8;
    static yarm_ovt_x86_k_end: u8;
}

static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Release);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

fn image(start: *const u8, end: *const u8) -> alloc::vec::Vec<u8> {
    let len = end as usize - start as usize;
    // SAFETY: `start..end` is the program's bytes inside this kernel image's text.
    unsafe { core::slice::from_raw_parts(start, len) }.to_vec()
}

/// The waiter's program (the client, CPU 0).
pub(crate) fn w_image() -> alloc::vec::Vec<u8> {
    // SAFETY: taking the address of an extern static reads nothing.
    image(
        &raw const yarm_ovt_x86_w_start,
        &raw const yarm_ovt_x86_w_end,
    )
}

/// The waker's program (the server, CPU 1).
pub(crate) fn k_image() -> alloc::vec::Vec<u8> {
    // SAFETY: taking the address of an extern static reads nothing.
    image(
        &raw const yarm_ovt_x86_k_start,
        &raw const yarm_ovt_x86_k_end,
    )
}
