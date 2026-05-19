// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::SyscallReturn;

#[inline]
pub(crate) unsafe fn raw_syscall(no: usize, args: [usize; 6]) -> SyscallReturn {
    // Separate input and output bindings for x0..x3 so the compiler is forced
    // to read the register value after svc rather than reusing the pre-svc
    // variable (which is the original arg and not the kernel return value).
    let x0_in = args[0];
    let x1_in = args[1];
    let x2_in = args[2];
    let x3_in = args[3];
    let x4 = args[4];
    let x5 = args[5];
    let x8 = no;
    let mut x0: usize;
    let mut x1: usize;
    let mut x2: usize;
    // SAFETY: Follows kernel aarch64 trap ABI with `svc #0`.
    unsafe {
        core::arch::asm!(
            "svc #0",
            inlateout("x0") x0_in => x0,
            inlateout("x1") x1_in => x1,
            inlateout("x2") x2_in => x2,
            inlateout("x3") x3_in => _,
            in("x4") x4,
            in("x5") x5,
            in("x8") x8,
            options(nostack),
        );
    }
    SyscallReturn {
        ret0: x0,
        ret1: x1,
        ret2: x2,
        error: 0,
    }
}
