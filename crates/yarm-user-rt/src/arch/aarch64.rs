// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::SyscallReturn;
#[cfg(target_arch = "aarch64")]
use core::arch::global_asm;

#[repr(C)]
struct Aarch64SyscallFrame {
    args: [usize; 6],
    rets: [usize; 6],
}

#[cfg(target_arch = "aarch64")]
global_asm!(
    r#"
    .text
    .align 2
    .global yarm_aarch64_raw_syscall_frame
    .type yarm_aarch64_raw_syscall_frame,%function
yarm_aarch64_raw_syscall_frame:
    // x0 = frame ptr, x1 = syscall number
    mov x12, x0
    mov x8, x1

    ldr x0, [x12, #0]
    ldr x1, [x12, #8]
    ldr x2, [x12, #16]
    ldr x3, [x12, #24]
    ldr x4, [x12, #32]
    ldr x5, [x12, #40]

    svc #0

    str x0, [x12, #48]
    str x1, [x12, #56]
    str x2, [x12, #64]
    str x3, [x12, #72]
    str x4, [x12, #80]
    str x5, [x12, #88]
    ret
    "#
);

#[cfg(target_arch = "aarch64")]
unsafe extern "C" {
    fn yarm_aarch64_raw_syscall_frame(frame: *mut Aarch64SyscallFrame, no: usize);
}

#[inline]
pub(crate) unsafe fn raw_syscall(no: usize, args: [usize; 6]) -> SyscallReturn {
    let mut frame = Aarch64SyscallFrame {
        args,
        rets: [usize::MAX; 6],
    };
    // SAFETY: C-ABI shim fully controls syscall argument/return lanes and
    // writes post-svc x0..x5 into `frame.rets`.
    unsafe { yarm_aarch64_raw_syscall_frame(&mut frame as *mut Aarch64SyscallFrame, no) };
    SyscallReturn {
        ret0: frame.rets[0],
        ret1: frame.rets[1],
        ret2: frame.rets[2],
        ret3: frame.rets[3],
        ret4: frame.rets[4],
        ret5: frame.rets[5],
        error: 0,
    }
}
