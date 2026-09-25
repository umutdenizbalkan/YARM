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

/// U9-TIMER5 §3 — a syscall whose CALLEE-SAVED registers carry sentinels across the block.
///
/// The idle-boundary resume does not return through the register file the caller was using: the
/// task blocked, the CPU parked at its halt loop, and a later timer selected the task and restored
/// its whole GPR snapshot from the TCB (`write_task_gprs_to_saved_regs`). So "the registers came
/// back" is a claim about the CAPTURE as much as about the restore, and an ordinary wrapper cannot
/// test it: the compiler is free to spill anything it wants around a call, and a value that came
/// back from the stack proves nothing about the snapshot.
///
/// The seed and the read-back therefore both happen through EXPLICIT register operands on the one
/// asm block that contains the `syscall`. `inlateout` on a named register is what makes that
/// airtight: the value is in that physical register when the syscall executes, and the value read
/// afterwards is whatever that physical register holds — there is no intervening instruction the
/// compiler could satisfy from a spill slot instead.
///
/// It is written this way rather than with in-block comparisons because the comparison form needs
/// scratch registers, and this block has none to give: seven explicit argument registers plus
/// `RCX`/`R11` plus these four leaves the allocator nothing, and `RBX` and `RBP` are both reserved
/// by LLVM and cannot be named at all.
///
/// System V names six callee-saved registers; these are the four that an inline asm block may
/// touch. That is a limit on the instrument, not on the claim —
/// `write_task_gprs_to_saved_regs` restores one flat snapshot, so four registers coming back
/// correct and another coming back wrong is not a reachable state of that code.
#[inline(never)]
pub(crate) unsafe fn raw_syscall_checking_callee_saved(
    no: usize,
    args: [usize; 6],
    sentinel: u64,
) -> (SyscallReturn, u32) {
    let mut ret0 = no;
    let ret1: usize;
    let mut ret2 = args[2];
    let error: usize;
    let s = sentinel as usize;
    // Distinct per register, so a restore that SHUFFLES the file is caught as well as one that
    // drops it.
    let (mut c0, mut c1, mut c2, mut c3) =
        (s, s.wrapping_add(1), s.wrapping_add(2), s.wrapping_add(3));
    // SAFETY: the same LSTAR ABI `raw_syscall` documents. The four extra operands are named
    // callee-saved registers carried in and out; the compiler preserves its own uses of them
    // around the block exactly as it does for any other explicit-register operand.
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") ret0,
            in("rdi") args[0],
            in("rsi") args[1],
            inlateout("rdx") ret2,
            in("r10") args[3],
            inlateout("r8") args[4] => ret1,
            in("r9") args[5],
            lateout("rcx") error,
            lateout("r11") _,
            inlateout("r12") c0,
            inlateout("r13") c1,
            inlateout("r14") c2,
            inlateout("r15") c3,
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
    (
        SyscallReturn {
            ret0,
            ret1,
            ret2,
            ret3: 0,
            ret4: 0,
            ret5: 0,
            error,
        },
        mask,
    )
}

/// How many callee-saved registers [`raw_syscall_checking_callee_saved`] seeds and verifies.
pub(crate) const CALLEE_SAVED_CHECKED: u32 = 4;

/// U9-PAGEFAULT1 §3 — store `value` through `addr`, then read it back, with four callee-saved
/// registers seeded and verified ACROSS the access.
///
/// The access is expected to FAULT: `addr` names a page inside a demand-backed window that
/// `VmBrk` grew lazily, so the store takes a `#PF` the kernel must recover before the instruction
/// can complete.
///
/// Returns `(read_back, preserved_mask)`. Three independent facts fall out of it:
///
/// * **the faulting instruction retried** — if it had been skipped, or resumed past, the store
///   would never have landed and `read_back` would not be `value`;
/// * **the source register survived** — `read_back` is the value the store's source operand held,
///   so a clobbered file would write the wrong bytes rather than none;
/// * **the rest of the file survived** — the four sentinels are carried in and out in named
///   callee-saved registers and compared on the far side, each distinct so a restore that
///   SHUFFLES the file is caught as well as one that drops it.
///
/// `rbx` is deliberately not among them: LLVM reserves it internally and it cannot be named in
/// Rust inline asm.
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
    // for a `u64`. The store is the faulting access; the load that follows reads the same slot
    // back in the same block, so no intervening code can perturb it.
    unsafe {
        core::arch::asm!(
            "mov qword ptr [{addr}], {val}",
            "mov {out}, qword ptr [{addr}]",
            addr = in(reg) addr,
            val = in(reg) value,
            out = out(reg) read_back,
            inlateout("r12") c0,
            inlateout("r13") c1,
            inlateout("r14") c2,
            inlateout("r15") c3,
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

/// QEMU-BASELINE1 §3 — spin in userspace for `budget_cycles` of TSC with `r12`–`r15` holding
/// distinct sentinels for the whole loop, counting TSC jumps of at least `gap_cycles`.
///
/// Returns `(gaps, elapsed_cycles, preserved_mask)`. Every interrupt that lands inside the loop
/// returns into it, so a resumed task with a clobbered or shuffled register file shows up in the
/// mask. `rbx` is not among them for the same reason as in `touch_checking_callee_saved`.
#[cfg(feature = "timer-contract-witness")]
pub(crate) unsafe fn spin_checking_callee_saved(
    budget_cycles: u64,
    gap_cycles: u64,
    sentinel: u64,
) -> Option<(u64, u64, u32)> {
    let s = sentinel as usize;
    let (mut c0, mut c1, mut c2, mut c3) =
        (s, s.wrapping_add(1), s.wrapping_add(2), s.wrapping_add(3));
    let gaps: u64;
    let elapsed: u64;
    // SAFETY: reads the TSC and compares registers; no memory is touched and no stack is used.
    unsafe {
        core::arch::asm!(
            "rdtsc",
            "shl rdx, 32",
            "or rax, rdx",
            "mov {start}, rax",
            "mov {prev}, rax",
            "xor {gaps:e}, {gaps:e}",
            "2:",
            "pause",
            "rdtsc",
            "shl rdx, 32",
            "or rax, rdx",
            "mov {d}, rax",
            "sub {d}, {prev}",
            "mov {prev}, rax",
            "cmp {d}, {gap}",
            "jb 3f",
            "inc {gaps}",
            "3:",
            "mov {d}, rax",
            "sub {d}, {start}",
            "cmp {d}, {budget}",
            "jb 2b",
            start = out(reg) _,
            prev = out(reg) _,
            gaps = out(reg) gaps,
            d = out(reg) elapsed,
            gap = in(reg) gap_cycles,
            budget = in(reg) budget_cycles,
            out("rax") _,
            out("rdx") _,
            inlateout("r12") c0,
            inlateout("r13") c1,
            inlateout("r14") c2,
            inlateout("r15") c3,
            options(nomem, nostack),
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
    Some((gaps, elapsed, mask))
}
