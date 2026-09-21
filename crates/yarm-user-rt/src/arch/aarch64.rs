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

/// U9-TIMER5 §3 — a syscall whose CALLEE-SAVED registers carry sentinels across the block.
///
/// See the x86_64 sibling for why the seed and the read-back both go through EXPLICIT register
/// operands on the one block that contains the `svc`: the idle-boundary resume restores the whole
/// GPR snapshot from the TCB rather than returning through the caller's live register file, so a
/// value that came back from a compiler spill would prove nothing about the snapshot.
///
/// `x20..x23` are AAPCS64 callee-saved and are the ones an inline asm block may touch here. `x18`
/// is the platform register — it carries this task's TLS base and is restored by the resume's own
/// TLS step, so seeding it would test the wrong owner — `x19` is reserved by LLVM as its own base
/// pointer, and `x29`/`x30` are the frame pointer and link register. That is a limit on the
/// instrument, not on the claim: the resume restores one flat snapshot, so four registers coming
/// back correct and another coming back wrong is not a reachable state of it.
#[inline(never)]
pub(crate) unsafe fn raw_syscall_checking_callee_saved(
    no: usize,
    args: [usize; 6],
    sentinel: u64,
) -> (SyscallReturn, u32) {
    let mut x0 = args[0];
    let mut x1 = args[1];
    let mut x2 = args[2];
    let mut x3 = args[3];
    let mut x4 = args[4];
    let mut x5 = args[5];
    let (r0, r1, r2): (usize, usize, usize);
    let s = sentinel as usize;
    // Distinct per register, so a restore that SHUFFLES the file is caught as well as one that
    // drops it.
    let (mut c0, mut c1, mut c2, mut c3) =
        (s, s.wrapping_add(1), s.wrapping_add(2), s.wrapping_add(3));
    // SAFETY: the same `svc #0` ABI `raw_syscall` uses, with four named callee-saved registers
    // carried in and out.
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
            inlateout("x20") c0,
            inlateout("x21") c1,
            inlateout("x22") c2,
            inlateout("x23") c3,
        );
    }
    let mut mask = 0u32;
    if c0 == s {
        mask |= 1;
    }
    if c1 == s.wrapping_add(1) {
        mask |= 2;
    }
    if c2 == s.wrapping_add(2) {
        mask |= 4;
    }
    if c3 == s.wrapping_add(3) {
        mask |= 8;
    }
    (
        SyscallReturn {
            ret0: r0,
            ret1: r1,
            ret2: r2,
            ret3: 0,
            ret4: 0,
            ret5: 0,
            error: 0,
        },
        mask,
    )
}

/// How many callee-saved registers [`raw_syscall_checking_callee_saved`] seeds and verifies.
pub(crate) const CALLEE_SAVED_CHECKED: u32 = 4;

/// U9-PAGEFAULT1 §3 — the AArch64 twin of the x86_64 helper. See that one for what the three
/// facts it returns are and why each matters.
///
/// `x20..x23` are the AAPCS64 callee-saved registers an inline asm block may name here.
#[cfg(feature = "pagefault1-demand-witness")]
pub(crate) unsafe fn touch_checking_callee_saved(
    addr: usize,
    value: u64,
    sentinel: u64,
) -> (u64, u32) {
    let read_back: u64;
    let s = sentinel as usize;
    let (mut c0, mut c1, mut c2, mut c3) =
        (s, s.wrapping_add(1), s.wrapping_add(2), s.wrapping_add(3));
    // SAFETY: `addr` is inside this task's own brk window, page-aligned by the caller and sized
    // for a `u64`. The store is the faulting access; the load reads the same slot back in the
    // same block.
    unsafe {
        core::arch::asm!(
            "str {val}, [{addr}]",
            "ldr {out}, [{addr}]",
            addr = in(reg) addr,
            val = in(reg) value,
            out = out(reg) read_back,
            inlateout("x20") c0,
            inlateout("x21") c1,
            inlateout("x22") c2,
            inlateout("x23") c3,
            options(nostack),
        );
    }
    let mut mask = 0u32;
    if c0 == s {
        mask |= 1;
    }
    if c1 == s.wrapping_add(1) {
        mask |= 2;
    }
    if c2 == s.wrapping_add(2) {
        mask |= 4;
    }
    if c3 == s.wrapping_add(3) {
        mask |= 8;
    }
    (read_back, mask)
}
