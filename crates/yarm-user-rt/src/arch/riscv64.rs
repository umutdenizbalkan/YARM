// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::SyscallReturn;

#[inline]
pub(crate) unsafe fn raw_syscall(no: usize, args: [usize; 6]) -> SyscallReturn {
    let mut a0 = args[0];
    let mut a1 = args[1];
    let mut a2 = args[2];
    let a3 = args[3];
    let a4 = args[4];
    let a5 = args[5];
    let a7 = no;
    // SAFETY: Follows kernel riscv64 trap ABI with `ecall`.
    unsafe {
        core::arch::asm!(
            "ecall",
            inlateout("a0") a0,
            inlateout("a1") a1,
            inlateout("a2") a2,
            in("a3") a3,
            in("a4") a4,
            in("a5") a5,
            in("a7") a7,
            options(nostack),
        );
    }
    SyscallReturn {
        ret0: a0,
        ret1: a1,
        ret2: a2,
        ret3: 0,
        ret4: 0,
        ret5: 0,
        error: 0,
    }
}

/// U9-TIMER5 §3 — the RISC-V stand-in.
///
/// RISC-V is NOT a changed port in U9-TIMER5: its idle-boundary landing predates this package and
/// CONSTRUCTS a user return from the S-mode timer entry rather than converting a kernel frame. The
/// witness therefore runs its cycle here without the register bracket, and reports `checked=0` so
/// a reader can tell "not verified on this port" from "verified and intact".
#[inline]
pub(crate) unsafe fn raw_syscall_checking_callee_saved(
    no: usize,
    args: [usize; 6],
    _sentinel: u64,
) -> (SyscallReturn, u32) {
    // SAFETY: delegates to the ordinary ecall path with the caller's own arguments.
    (unsafe { raw_syscall(no, args) }, 0)
}

/// Zero: this port seeds and verifies no callee-saved register (see above).
pub(crate) const CALLEE_SAVED_CHECKED: u32 = 0;
