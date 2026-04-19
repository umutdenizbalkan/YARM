// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const COM1_PORT: u16 = 0x3F8;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const COM1_LINE_STATUS: u16 = COM1_PORT + 5;
#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
const LINE_STATUS_THR_EMPTY: u8 = 1 << 5;

#[cfg(feature = "hosted-dev")]
pub fn write_line(_msg: &str) {}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn write_line(msg: &str) {
    for &byte in msg.as_bytes() {
        if byte == b'\n' {
            write_byte(b'\r');
        }
        write_byte(byte);
    }
    write_byte(b'\r');
    write_byte(b'\n');
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
pub fn write_breadcrumb(byte: u8) {
    // Raw COM1 breadcrumb for -serial output capture (no formatting/allocation).
    while (inb(COM1_LINE_STATUS) & LINE_STATUS_THR_EMPTY) == 0 {}
    outb(COM1_PORT, byte);
    // Keep debugcon breadcrumb as secondary channel when enabled.
    outb(0xE9, byte);
}

#[cfg(feature = "hosted-dev")]
pub fn write_breadcrumb(_byte: u8) {}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn write_byte(byte: u8) {
    while (inb(COM1_LINE_STATUS) & LINE_STATUS_THR_EMPTY) == 0 {}
    outb(COM1_PORT, byte);
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn outb(port: u16, value: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "x86_64"))]
fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

#[cfg(all(not(feature = "hosted-dev"), not(target_arch = "x86_64")))]
pub fn write_line(_msg: &str) {}
