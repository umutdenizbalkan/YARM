// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::SyscallReturn;

#[inline(never)]
pub(crate) unsafe fn raw_syscall(no: usize, args: [usize; 6]) -> SyscallReturn {
    // Copy the parameter-by-value array into a local so its address is a
    // real stable stack location.  Without this, the by-value `args` may be
    // promoted into registers and `args.as_ptr()` would return the address
    // of a value that was never spilled, causing the asm loads to read
    // stale memory.
    let args_mem: [usize; 6] = args;
    let mut r0: usize;
    let mut r1: usize;
    let mut r2: usize;
    // SAFETY: Follows kernel aarch64 trap ABI with `svc #0`.
    //
    // Register layout:
    // - x9  = args_mem.as_ptr() (pinned input, never an output, kernel
    //         preserves x9 across the syscall)
    // - x10 = syscall number (pinned input, never an output, kernel
    //         preserves x10)
    // - x0..x5 are loaded from memory inside the asm body, then SVC.
    //   After SVC, the kernel writes ret0/ret1/ret2 into x0/x1/x2 via the
    //   vector frame.  All seven (x0..x5, x8) are pure `lateout` so the
    //   compiler has no pre-asm input it could substitute for the post-asm
    //   value.
    //
    // No `options(nostack)`: the asm reads from a stack-allocated array
    // (`args_mem`), so we leave the default options which let the compiler
    // assume the asm may read/write memory and stack as needed.  This
    // ensures `args_mem` is properly materialized to memory before x9 is
    // computed.
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
            in("x9")  args_mem.as_ptr(),
            in("x10") no,
            lateout("x0") r0,
            lateout("x1") r1,
            lateout("x2") r2,
            lateout("x3") _,
            lateout("x4") _,
            lateout("x5") _,
            lateout("x8") _,
        );
    }
    SyscallReturn {
        ret0: r0,
        ret1: r1,
        ret2: r2,
        error: 0,
    }
}
