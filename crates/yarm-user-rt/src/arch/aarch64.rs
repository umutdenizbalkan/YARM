// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::SyscallReturn;

#[inline]
pub(crate) unsafe fn raw_syscall(no: usize, args: [usize; 6]) -> SyscallReturn {
    let mut r0: usize;
    let mut r1: usize;
    let mut r2: usize;
    // SAFETY: Follows kernel aarch64 trap ABI with `svc #0`.
    //
    // Using in(reg) for inputs and lateout for outputs avoids any inlateout
    // tied-constraint that would allow LLVM to substitute the pre-svc input
    // value for the output (a CSE / value-forwarding misoptimisation observed
    // with inlateout("x1") x_in => x_out when the function is inlined).
    //
    // The compiler allocates a0..a5/nr to arbitrary caller-saved registers
    // (not x0-x5 or x8 since those are declared lateout/clobber), then the
    // explicit mov instructions load them into the ABI-mandated positions
    // before svc.  After svc, lateout reads x0/x1/x2 into the fresh,
    // input-unrelated r0/r1/r2 variables.
    unsafe {
        core::arch::asm!(
            "mov x0, {a0}",
            "mov x1, {a1}",
            "mov x2, {a2}",
            "mov x3, {a3}",
            "mov x4, {a4}",
            "mov x5, {a5}",
            "mov x8, {nr}",
            "svc #0",
            a0 = in(reg) args[0],
            a1 = in(reg) args[1],
            a2 = in(reg) args[2],
            a3 = in(reg) args[3],
            a4 = in(reg) args[4],
            a5 = in(reg) args[5],
            nr = in(reg) no,
            lateout("x0") r0,
            lateout("x1") r1,
            lateout("x2") r2,
            lateout("x3") _,
            lateout("x4") _,
            lateout("x5") _,
            lateout("x8") _,
            options(nostack),
        );
    }
    SyscallReturn {
        ret0: r0,
        ret1: r1,
        ret2: r2,
        error: 0,
    }
}
