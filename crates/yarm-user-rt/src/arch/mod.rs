// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SyscallReturn {
    pub(crate) ret0: usize,
    pub(crate) ret1: usize,
    pub(crate) ret2: usize,
    pub(crate) error: usize,
}

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::{raw_syscall, serial_write_bytes};

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::raw_syscall;
#[cfg(target_arch = "aarch64")]
pub(crate) fn serial_write_bytes(_bytes: &[u8]) {}

#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "riscv64")]
pub(crate) use riscv64::raw_syscall;
#[cfg(target_arch = "riscv64")]
pub(crate) fn serial_write_bytes(_bytes: &[u8]) {}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "riscv64")))]
mod unsupported;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "riscv64")))]
pub(crate) use unsupported::raw_syscall;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "riscv64")))]
pub(crate) fn serial_write_bytes(_bytes: &[u8]) {}
