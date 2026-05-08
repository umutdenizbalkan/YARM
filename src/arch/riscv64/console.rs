// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(feature = "hosted-dev")]
pub fn write_line(_msg: &str) {}
#[cfg(feature = "hosted-dev")]
pub fn write_byte(_byte: u8) {}
#[cfg(feature = "hosted-dev")]
pub fn try_write_byte(_byte: u8) -> bool {
    false
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub fn write_line(msg: &str) {
    for &byte in msg.as_bytes() {
        if byte == b'\n' {
            write_byte_impl(b'\r');
        }
        write_byte_impl(byte);
    }
    write_byte_impl(b'\r');
    write_byte_impl(b'\n');
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
fn write_byte_impl(byte: u8) {
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

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub fn write_byte_public(byte: u8) {
    write_byte_impl(byte);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub fn try_write_byte(byte: u8) -> bool {
    write_byte_public(byte);
    true
}

#[cfg(all(not(feature = "hosted-dev"), not(target_arch = "riscv64")))]
pub fn write_line(_msg: &str) {}
#[cfg(all(not(feature = "hosted-dev"), not(target_arch = "riscv64")))]
pub fn write_byte(_byte: u8) {}
#[cfg(all(not(feature = "hosted-dev"), not(target_arch = "riscv64")))]
pub fn try_write_byte(_byte: u8) -> bool {
    false
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
pub use write_byte_public as write_byte;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosted_console_write_is_noop_safe() {
        #[cfg(feature = "hosted-dev")]
        write_line("riscv64-console");
    }
}
