// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::SyscallReturn;

/// x86-64 syscall entry register convention used by YARM.
///
/// Argument mapping (in → kernel sees via LSTAR entry):
///   arg0 → RDI   arg1 → RSI   arg2 → RDX
///   arg3 → R10   (NOT RCX: the SYSCALL instruction clobbers RCX, replacing
///                it with the return RIP.  The kernel's LSTAR entry recovers
///                arg3 with `mov rcx, r10` before pushing the GPR frame.)
///   arg4 → R8    arg5 → R9
///   syscall number → RAX
///
/// Return values (kernel → out via IRETQ pop sequence):
///   ret0  ← RAX   (write_trap_returns_to_saved_regs sets regs.rax = ret0)
///   ret1  ← R8    (                              ...  regs.r8  = ret1)
///   ret2  ← RDX   (                              ...  regs.rdx = ret2)
///   error ← RCX   (                              ...  regs.rcx = error)
///
/// Note: R11 is always clobbered by SYSCALL (hardware saves RFLAGS there).
/// Note: R8 carries both arg4 (input) and ret1 (output).  `inlateout("r8")`
///       is valid because lateout outputs are only written after all inputs
///       are consumed.  R8 is caller-saved in the System V ABI, so the
///       compiler will not keep live values there across the syscall boundary.
/// Note: RBX is callee-saved in the System V ABI.  The kernel intentionally
///       does NOT write ret1 to the saved RBX slot, so IRETQ restores the
///       original user RBX, leaving callee-saved state intact.
#[inline]
pub(crate) unsafe fn raw_syscall(no: usize, args: [usize; 6]) -> SyscallReturn {
    let mut ret0 = no;
    let ret1: usize;
    let mut ret2 = args[2];
    let error: usize;
    // SAFETY: Follows the kernel x86-64 LSTAR syscall ABI.
    //
    // arg3 is passed in R10 — NEVER in RCX.  The SYSCALL instruction
    // unconditionally overwrites RCX with the user-mode return RIP, so any
    // value placed in RCX before SYSCALL is silently destroyed.  The kernel's
    // LSTAR entry does `mov rcx, r10` to forward arg3 to the trap-frame rcx
    // slot, so R10 is the correct vehicle for arg3 on x86-64.
    //
    // R8 is used for both arg4 (input) and ret1 (output) via inlateout.
    // This is safe: inlateout("r8") arg4 => ret1 ensures arg4 is in R8 when
    // the SYSCALL executes; after the kernel returns (via IRETQ), the kernel's
    // write_trap_returns_to_saved_regs has placed ret1 in the saved R8 slot,
    // so R8 holds ret1.  R8 is caller-saved (System V ABI), so it is safe
    // for the kernel to overwrite it on return.
    //
    // RBX is callee-saved.  The kernel does NOT write to the saved RBX slot,
    // so IRETQ restores the original user RBX, preserving callee-saved state.
    // No lateout("rbx") declaration is needed because we don't clobber it.
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") ret0,
            in("rdi") args[0],          // arg0
            in("rsi") args[1],          // arg1
            inlateout("rdx") ret2,      // arg2 in, ret2 out
            in("r10") args[3],          // arg3 (NOT rcx — SYSCALL clobbers RCX)
            inlateout("r8") args[4] => ret1,  // arg4 in, ret1 out (R8 is caller-saved)
            in("r9")  args[5],          // arg5
            // RCX on return: the kernel wrote the error code into regs.rcx via
            // write_trap_returns_to_saved_regs; IRETQ pops it so RCX is the error.
            lateout("rcx") error,
            lateout("r11") _,           // SYSCALL always clobbers R11 (saves RFLAGS)
            options(nostack),
        );
    }
    SyscallReturn {
        ret0,
        ret1,
        ret2,
        ret3: 0,
        ret4: 0,
        ret5: 0,
        error,
    }
}
