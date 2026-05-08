// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::SyscallReturn;
const SYSCALL_DEBUG_SERIAL_WRITE_NR: usize = 21;

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
        error: 0,
    }
}

#[inline]
pub(crate) fn serial_write_bytes(bytes: &[u8]) {
    // Kernel-side RISC-V console path currently emits through SBI rather than a
    // userspace-mapped UART MMIO window; use syscall bridge for serial markers.
    for &byte in bytes {
        // SAFETY: fixed debug-serial syscall ABI; argument is a single byte lane.
        let _ = unsafe { raw_syscall(SYSCALL_DEBUG_SERIAL_WRITE_NR, [byte as usize, 0, 0, 0, 0, 0]) };
    }
}
