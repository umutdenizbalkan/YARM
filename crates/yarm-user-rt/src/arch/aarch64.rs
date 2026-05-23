// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::SyscallReturn;

#[repr(C)]
struct Aarch64SyscallFrame {
    args: [usize; 6],
    rets: [usize; 6],
}

#[inline]
pub(crate) unsafe fn raw_syscall(no: usize, args: [usize; 6]) -> SyscallReturn {
    let mut frame = Aarch64SyscallFrame {
        args,
        rets: [usize::MAX; 6],
    };
    let mut frame_ptr = (&mut frame as *mut Aarch64SyscallFrame) as usize;
    // SAFETY: Follows kernel aarch64 trap ABI with `svc #0`.
    //
    // Load x0..x5 from a single frame pointer pinned in x12, then store
    // post-svc x0..x5 back into that same frame. This avoids input register
    // write-order/overlap hazards from in("x0")..in("x5") constraints.
    unsafe {
        core::arch::asm!(
            "ldr x0, [x12, #0]",
            "ldr x1, [x12, #8]",
            "ldr x2, [x12, #16]",
            "ldr x3, [x12, #24]",
            "ldr x4, [x12, #32]",
            "ldr x5, [x12, #40]",
            "svc #0",
            "str x0, [x12, #48]",
            "str x1, [x12, #56]",
            "str x2, [x12, #64]",
            "str x3, [x12, #72]",
            "str x4, [x12, #80]",
            "str x5, [x12, #88]",
            inout("x12") frame_ptr => _,
            in("x8") no,
            options(nostack),
        );
    }
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
