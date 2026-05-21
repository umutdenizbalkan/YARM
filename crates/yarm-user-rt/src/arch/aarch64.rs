// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::SyscallReturn;

#[inline]
pub(crate) unsafe fn raw_syscall(no: usize, args: [usize; 6]) -> SyscallReturn {
    let mut x0 = args[0];
    let mut x1 = args[1];
    let mut x2 = args[2];
    let mut x3 = args[3];
    let mut x4 = args[4];
    let mut x5 = args[5];
    let mut r0: usize;
    let mut r1: usize;
    let mut r2: usize;
    // Diagnostic captures: save post-svc x3/x4/x5 to detect kernel register-restore bugs.
    let mut r3: usize;
    let mut r4: usize;
    let mut r5: usize;
    // SAFETY: Follows kernel aarch64 trap ABI with `svc #0`.
    //
    // The svc instruction uses x0..x5 as in/out and x8 as the syscall
    // number.  Immediately after svc, the post-svc register values are
    // copied into compiler-allocated scratch registers (r0..r5) via
    // explicit mov instructions inside the same asm block.  This prevents
    // the compiler from reading x0/x1/x2 directly and substituting a
    // pre-svc cached value — a hazard that bit all previous approaches
    // (lateout, inout without moves).
    //
    // r3/r4/r5 capture the kernel-restored x3/x4/x5 values (which should
    // equal the original args[3..5]) for diagnostic logging only.
    unsafe {
        core::arch::asm!(
            "svc #0",
            "mov {r0}, x0",
            "mov {r1}, x1",
            "mov {r2}, x2",
            "mov {r3}, x3",
            "mov {r4}, x4",
            "mov {r5}, x5",
            r0 = lateout(reg) r0,
            r1 = lateout(reg) r1,
            r2 = lateout(reg) r2,
            r3 = lateout(reg) r3,
            r4 = lateout(reg) r4,
            r5 = lateout(reg) r5,
            inout("x0") x0 => _,
            inout("x1") x1 => _,
            inout("x2") x2 => _,
            inout("x3") x3 => _,
            inout("x4") x4 => _,
            inout("x5") x5 => _,
            in("x8") no,
        );
    }
    SyscallReturn {
        ret0: r0,
        ret1: r1,
        ret2: r2,
        ret3: r3,
        ret4: r4,
        ret5: r5,
        error: 0,
    }
}
