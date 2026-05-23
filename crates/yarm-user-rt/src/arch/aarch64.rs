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
    let (r0, r1, r2): (usize, usize, usize);
    unsafe {
        core::arch::asm!(
            "svc #0",
            "mov {r0}, x0",
            "mov {r1}, x1",
            "mov {r2}, x2",
            r0 = lateout(reg) r0,
            r1 = lateout(reg) r1,
            r2 = lateout(reg) r2,
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
        ret3: 0,
        ret4: 0,
        ret5: 0,
        error: 0,
    }
}
