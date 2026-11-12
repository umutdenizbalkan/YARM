//! RISCV-ECALL-A7-RET1 — the kernel half of the live `ecall` register-contract witness
//! (default-off, `riscv-ecall-witness`).
//!
//! The userspace witness issues each probed `ecall` with a KEY in `a6` — a register the syscall
//! ABI does not use and every resume arm restores from the saved register file. Each write-back
//! calls [`note_resume`] after it has written the ABI lanes; for a keyed continuation it prints
//! which arm resumed it and the registers that arm installed, so the grader can pair every
//! userspace observation with the exact kernel decision that produced it. Nothing here changes
//! a register, a decision or an ordering: the marker reads the finished frame.

use super::boot::RiscvTrapFrame;

/// The high half of every witness key (`0xEA7C_0DE5`).
pub(crate) const KEY_HI: u64 = 0xEA7C_0DE5;
/// `a6` (x16) in `RiscvTrapFrame::regs`.
const A6: usize = 15;

/// The arm a write-back chose, from the same decisions it chose it with.
pub(crate) fn arm(task_switched: bool, async_resume: bool, continuation: bool) -> &'static str {
    if task_switched && async_resume {
        "async"
    } else if task_switched && continuation {
        "continuation"
    } else if task_switched {
        "switched"
    } else {
        "same"
    }
}

/// One marker per keyed resume.
pub(crate) fn note_resume(origin: &str, arm: &str, tid: u64, f: &RiscvTrapFrame) {
    let key = f.regs[A6];
    if key >> 32 != KEY_HI {
        return;
    }
    // Kept under the kernel log's line limit: the grader pairs on `key` and compares `sepc`, `a0`,
    // `a3`, `a7` (userspace checks every other register itself).
    crate::yarm_log!(
        "RISCV_ECALL_WITNESS_RESUME origin={} arm={} tid={} key=0x{:x} sepc=0x{:x} a0=0x{:x} a3=0x{:x} a7=0x{:x}",
        origin,
        arm,
        tid,
        key,
        f.sepc,
        f.regs[RiscvTrapFrame::A0],
        f.regs[RiscvTrapFrame::A3],
        f.regs[RiscvTrapFrame::A7],
    );
}
