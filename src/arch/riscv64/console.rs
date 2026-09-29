// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(feature = "hosted-dev")]
pub fn write_line(_msg: &str) {}

#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "riscv64",
    not(feature = "riscv64-smp3-witness")
))]
pub fn write_line(msg: &str) {
    write_line_bytes(msg);
}

/// QEMU-SMP3 witness build only — one line at a time across both harts.
///
/// Two dispatching harts write the SBI console a byte per `ecall` with no shared line lock, so
/// their lines interleave byte-wise and the witness's sealed dump arrives damaged. The witness
/// build serializes whole lines here, with this hart's S-mode interrupts masked across the hold
/// (the same discipline `early_sbi_marker` uses, and for the same reason: a handler on this hart
/// that prints must never spin on a lock this hart holds). The spin is bounded, so a hart that
/// stopped while holding the line — a panic mid-line — cannot silence the other one. Default
/// builds are unchanged.
#[cfg(all(
    not(feature = "hosted-dev"),
    target_arch = "riscv64",
    feature = "riscv64-smp3-witness"
))]
pub fn write_line(msg: &str) {
    use core::sync::atomic::{AtomicBool, Ordering};
    static LINE: AtomicBool = AtomicBool::new(false);
    let irq_state = crate::arch::riscv64::irq::irq_save();
    let mut spins = 0u32;
    let held = loop {
        if LINE
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            break true;
        }
        spins += 1;
        if spins >= 50_000_000 {
            break false;
        }
        core::hint::spin_loop();
    };
    write_line_bytes(msg);
    if held {
        LINE.store(false, Ordering::Release);
    }
    crate::arch::riscv64::irq::irq_restore(irq_state);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn write_line_bytes(msg: &str) {
    for &byte in msg.as_bytes() {
        if byte == b'\n' {
            write_byte(b'\r');
        }
        write_byte(byte);
    }
    write_byte(b'\r');
    write_byte(b'\n');
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn write_byte(byte: u8) {
    // Legacy SBI console_putchar (a7=1, a0=char, ecall).
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a0") byte as usize,
            in("a7") 1usize,
            options(nostack, preserves_flags)
        );
    }
}

#[cfg(all(not(feature = "hosted-dev"), not(target_arch = "riscv64")))]
pub fn write_line(_msg: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosted_console_write_is_noop_safe() {
        #[cfg(feature = "hosted-dev")]
        write_line("riscv64-console");
    }
}
