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
    // All previous approaches (inlateout, in(reg)+lateout) suffered from the
    // compiler allocating an input to the same physical register as a lateout
    // output, letting LLVM prove "output == input" and substitute the pre-svc
    // value.
    //
    // This version pins args_ptr to x9 and the syscall number to x10 — two
    // registers that are never declared as outputs and that the YARM kernel
    // preserves across a syscall (only x0-x5 and x15/TLS are overwritten by
    // restore_arch_thread_state / export_syscall_result_to_user_gprs).
    // The asm body loads x0..x5 from the array in memory and moves x10 -> x8,
    // so no input register can alias x0/x1/x2.  r0/r1/r2 are pure lateout
    // with no prior value; the compiler must read them from registers post-svc.
    unsafe {
        core::arch::asm!(
            "ldr x0, [x9, #0]",
            "ldr x1, [x9, #8]",
            "ldr x2, [x9, #16]",
            "ldr x3, [x9, #24]",
            "ldr x4, [x9, #32]",
            "ldr x5, [x9, #40]",
            "mov x8, x10",
            "svc #0",
            in("x9")  args.as_ptr(),
            in("x10") no,
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
