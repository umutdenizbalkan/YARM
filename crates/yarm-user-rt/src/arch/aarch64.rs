// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::SyscallReturn;

#[inline]
pub(crate) unsafe fn raw_syscall(no: usize, args: [usize; 6]) -> SyscallReturn {
    let mut ret = [usize::MAX; 6];
    let ret_ptr = ret.as_mut_ptr();
    // SAFETY: Follows kernel aarch64 trap ABI with `svc #0`.
    //
    // We store x0..x5 directly to memory in the same asm block immediately
    // after `svc`. This avoids LLVM register-allocation overlap hazards seen
    // with inout/lateout patterns for higher return lanes (especially x5).
    unsafe {
        core::arch::asm!(
            "svc #0",
            "str x0, [{ret_ptr}, #0]",
            "str x1, [{ret_ptr}, #8]",
            "str x2, [{ret_ptr}, #16]",
            "str x3, [{ret_ptr}, #24]",
            "str x4, [{ret_ptr}, #32]",
            "str x5, [{ret_ptr}, #40]",
            ret_ptr = in(reg) ret_ptr,
            in("x0") args[0],
            in("x1") args[1],
            in("x2") args[2],
            in("x3") args[3],
            in("x4") args[4],
            in("x5") args[5],
            in("x8") no,
        );
    }
    SyscallReturn {
        ret0: ret[0],
        ret1: ret[1],
        ret2: ret[2],
        ret3: ret[3],
        ret4: ret[4],
        ret5: ret[5],
        error: 0,
    }
}
