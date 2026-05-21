// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::SyscallReturn;

#[inline]
pub(crate) unsafe fn raw_syscall(_no: usize, _args: [usize; 6]) -> SyscallReturn {
    SyscallReturn {
        ret0: 0,
        ret1: 0,
        ret2: 0,
        ret3: 0,
        ret4: 0,
        ret5: 0,
        error: 1,
    }
}
