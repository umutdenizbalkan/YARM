// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(not(feature = "hosted-dev"))]
use crate::kernel::lock::SpinLockIrq;
#[cfg(not(feature = "hosted-dev"))]
use core::ptr::{read_volatile, write_volatile};
#[cfg(not(feature = "hosted-dev"))]
use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(not(feature = "hosted-dev"))]
const QEMU_VIRT_PL011_BASE: usize = 0x0900_0000;
#[cfg(not(feature = "hosted-dev"))]
const PL011_DR: usize = 0x000;
#[cfg(not(feature = "hosted-dev"))]
const PL011_FR: usize = 0x018;
#[cfg(not(feature = "hosted-dev"))]
const PL011_FR_TXFF: u32 = 1 << 5;

#[cfg(not(feature = "hosted-dev"))]
static UART_BASE: AtomicUsize = AtomicUsize::new(QEMU_VIRT_PL011_BASE);
#[cfg(not(feature = "hosted-dev"))]
static UART_LOG_LOCK: SpinLockIrq<()> = SpinLockIrq::new(());

#[cfg(feature = "hosted-dev")]
pub fn write_line(_msg: &str) {}

#[cfg(not(feature = "hosted-dev"))]
pub fn init_early_mmio_base(base: usize) {
    if base != 0 {
        UART_BASE.store(base, Ordering::Relaxed);
    }
}

#[cfg(not(feature = "hosted-dev"))]
pub fn write_line(msg: &str) {
    // Serialize full-line emission under an IRQ-safe lock so SMP CPUs and local
    // IRQ/exception re-entry cannot interleave UART bytes mid-line.
    let _guard = UART_LOG_LOCK.lock();
    for &byte in msg.as_bytes() {
        if byte == b'\n' {
            write_byte(b'\r');
        }
        write_byte(byte);
    }
    write_byte(b'\r');
    write_byte(b'\n');
}

#[cfg(not(feature = "hosted-dev"))]
fn write_byte(byte: u8) {
    let base = UART_BASE.load(Ordering::Relaxed);
    while (mmio_read32(base + PL011_FR) & PL011_FR_TXFF) != 0 {}
    mmio_write32(base + PL011_DR, byte as u32);
}

#[cfg(not(feature = "hosted-dev"))]
fn mmio_read32(addr: usize) -> u32 {
    unsafe { read_volatile(addr as *const u32) }
}

#[cfg(not(feature = "hosted-dev"))]
fn mmio_write32(addr: usize, value: u32) {
    unsafe { write_volatile(addr as *mut u32, value) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosted_console_write_is_noop_safe() {
        #[cfg(feature = "hosted-dev")]
        write_line("aarch64-console");
    }

    #[test]
    fn early_mmio_base_accepts_nonzero_base() {
        #[cfg(not(feature = "hosted-dev"))]
        {
            init_early_mmio_base(0x0900_1000);
            assert_eq!(UART_BASE.load(Ordering::Relaxed), 0x0900_1000);
            init_early_mmio_base(QEMU_VIRT_PL011_BASE);
        }
    }
}
