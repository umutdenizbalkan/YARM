// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::SyscallReturn;

#[inline]
pub(crate) unsafe fn raw_syscall(no: usize, args: [usize; 6]) -> SyscallReturn {
    let mut x0 = args[0];
    let mut x1 = args[1];
    let mut x2 = args[2];
    let x3 = args[3];
    let x4 = args[4];
    let x5 = args[5];
    let x8 = no;
    // SAFETY: Follows kernel aarch64 trap ABI with `svc #0`.
    unsafe {
        core::arch::asm!(
            "svc #0",
            inlateout("x0") x0,
            inlateout("x1") x1,
            inlateout("x2") x2,
            in("x3") x3,
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

#[inline]
pub(crate) fn serial_write_bytes(bytes: &[u8]) {
    // Kernel boot logs use QEMU virt PL011 at 0x0900_0000, but that MMIO page is
    // not guaranteed to be mapped into every userspace task. Route markers via a
    // narrow syscall instead of blind userspace MMIO writes.
    super::serial_write_bytes_via_syscall(bytes);
}
