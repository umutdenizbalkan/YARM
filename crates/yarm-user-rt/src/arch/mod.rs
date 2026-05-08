// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SyscallReturn {
    pub(crate) ret0: usize,
    pub(crate) ret1: usize,
    pub(crate) ret2: usize,
    pub(crate) error: usize,
}

const SYSCALL_DEBUG_SERIAL_WRITE_NR: usize = 21;
use core::sync::atomic::{AtomicI8, Ordering};
static DEBUG_SERIAL_AVAILABLE: AtomicI8 = AtomicI8::new(-1);

#[inline]
fn debug_serial_is_available() -> bool {
    match DEBUG_SERIAL_AVAILABLE.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            // SAFETY: fixed debug-serial syscall ABI; probe uses zero byte and reserved args.
            let probe = unsafe { raw_syscall(SYSCALL_DEBUG_SERIAL_WRITE_NR, [0, 0, 0, 0, 0, 0]) };
            let available = (probe.error == 0) && (probe.ret0 != 0);
            DEBUG_SERIAL_AVAILABLE.store(if available { 1 } else { 0 }, Ordering::Relaxed);
            available
        }
    }
}

#[inline]
pub(crate) fn serial_write_bytes_via_syscall(bytes: &[u8]) {
    if !debug_serial_is_available() {
        return;
    }
    for &byte in bytes {
        // SAFETY: fixed debug-serial syscall ABI; one-byte payload in arg0.
        let _ = unsafe { raw_syscall(SYSCALL_DEBUG_SERIAL_WRITE_NR, [byte as usize, 0, 0, 0, 0, 0]) };
    }
}

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::{raw_syscall, serial_write_bytes};

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::{raw_syscall, serial_write_bytes};

#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "riscv64")]
pub(crate) use riscv64::{raw_syscall, serial_write_bytes};

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "riscv64")))]
mod unsupported;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "riscv64")))]
pub(crate) use unsupported::raw_syscall;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "riscv64")))]
pub(crate) fn serial_write_bytes(_bytes: &[u8]) {}
