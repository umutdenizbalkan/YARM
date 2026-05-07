// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::SyscallReturn;

#[inline]
pub(crate) unsafe fn raw_syscall(no: usize, args: [usize; 6]) -> SyscallReturn {
    let mut ret0 = no;
    let mut ret1: usize;
    let mut ret2 = args[2];
    let mut error = args[3];
    // SAFETY: Follows kernel x86_64 syscall ABI register contract.
    unsafe {
        core::arch::asm!(
            "syscall",
            "mov {ret1_tmp}, rbx",
            inlateout("rax") ret0,
            in("rdi") args[0],
            in("rsi") args[1],
            inlateout("rdx") ret2,
            inlateout("rcx") error,
            in("r8") args[4],
            in("r9") args[5],
            ret1_tmp = lateout(reg) ret1,
            lateout("r11") _,
            options(nostack),
        );
    }
    SyscallReturn {
        ret0,
        ret1,
        ret2,
        error,
    }
}

#[inline]
pub(crate) fn serial_write_bytes(bytes: &[u8]) {
    for &b in bytes {
        // SAFETY: fixed COM1 I/O port write on x86_64 debug path.
        unsafe {
            core::arch::asm!(
                "out dx, al",
                in("dx") 0x3f8u16,
                in("al") b,
                options(nomem, nostack, preserves_flags)
            );
        }
    }
}
