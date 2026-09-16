// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::SyscallReturn;

#[inline]
pub(crate) unsafe fn raw_syscall(_no: usize, _args: [usize; 6]) -> SyscallReturn {
    SyscallReturn {
        ret0: 0,
        ret1: 0,
        ret2: 0,
        ret3: 0,
        ret4: 0,
        ret5: 0,
        error: 1,
    }
}

/// U9-TIMER5 §3 — the no-architecture stand-in; see the RISC-V variant for what `checked=0` means.
#[inline]
pub(crate) unsafe fn raw_syscall_checking_callee_saved(
    no: usize,
    args: [usize; 6],
    _sentinel: u64,
) -> (SyscallReturn, u32) {
    // SAFETY: delegates to the stub, which performs no syscall at all.
    (unsafe { raw_syscall(no, args) }, 0)
}

/// Zero: this build seeds and verifies no callee-saved register.
pub(crate) const CALLEE_SAVED_CHECKED: u32 = 0;
