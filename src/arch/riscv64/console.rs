// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(feature = "hosted-dev")]
pub fn write_line(_msg: &str) {}

/// One line at a time across every hart — THE console line lock, for every writer.
///
/// Two dispatching harts write the SBI console a byte per `ecall`, so without a shared line lock
/// their lines interleave byte-wise. QEMU-SMP3 introduced this for its witness build, whose sealed
/// dump arrived damaged; AP-SCHED1 makes it production, because a production boot now has two
/// printing harts and a torn marker is a line every log consumer miscounts. `early_sbi_marker`
/// writes through here too, so the two output paths exclude each other rather than holding two
/// different locks. The hold is with this hart's S-mode interrupts masked (a handler on this hart
/// that prints must never spin on a lock this hart holds), and the spin is bounded, so a hart
/// that stopped while holding the line — a panic mid-line — cannot silence the other one.
#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
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
