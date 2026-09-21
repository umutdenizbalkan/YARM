// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SyscallReturn {
    pub(crate) ret0: usize,
    pub(crate) ret1: usize,
    pub(crate) ret2: usize,
    /// Post-svc value of arg3 register (x3/a3 etc.) — used for diagnostics.
    pub(crate) ret3: usize,
    /// Post-svc value of arg4 register (x4/a4 etc.) — used for diagnostics.
    pub(crate) ret4: usize,
    /// Post-svc value of arg5 register (x5/a5 etc.) — used for diagnostics.
    pub(crate) ret5: usize,
    pub(crate) error: usize,
}

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(all(target_arch = "x86_64", feature = "pagefault1-demand-witness"))]
pub(crate) use x86_64::touch_checking_callee_saved;
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::{CALLEE_SAVED_CHECKED, raw_syscall, raw_syscall_checking_callee_saved};

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(all(target_arch = "aarch64", feature = "pagefault1-demand-witness"))]
pub(crate) use aarch64::touch_checking_callee_saved;
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::{CALLEE_SAVED_CHECKED, raw_syscall, raw_syscall_checking_callee_saved};

#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(all(target_arch = "riscv64", feature = "pagefault1-demand-witness"))]
pub(crate) use riscv64::touch_checking_callee_saved;
#[cfg(target_arch = "riscv64")]
pub(crate) use riscv64::{CALLEE_SAVED_CHECKED, raw_syscall, raw_syscall_checking_callee_saved};

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
)))]
mod unsupported;
#[cfg(all(
    not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    )),
    feature = "pagefault1-demand-witness"
))]
pub(crate) use unsupported::touch_checking_callee_saved;
#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
)))]
pub(crate) use unsupported::{
    CALLEE_SAVED_CHECKED, raw_syscall, raw_syscall_checking_callee_saved,
};
