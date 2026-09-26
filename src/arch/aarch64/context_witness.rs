// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-CONTEXT1 §3 — the AArch64 half of the execution-state witness's kernel hooks.
//!
//! * [`check_kernel_env`] — entered from EL0, is the kernel running under its own FPCR (`0`), or
//!   under the interrupted task's rounding/flush-to-zero configuration?
//! * [`note_entering`] / [`note_idle_divergence`] — the task an EL0 exception interrupted, so a
//!   trap that diverges into the idle loop (and therefore never returns to the vector tail) still
//!   leaves its block transition in the log.
//! * [`clobber_user_visible_state`] (`context1-clobber`) — after dispatch, overwrite
//!   `q0..q31`, FPCR, FPSR, NZCV and TPIDR_EL0 with junk. It saves and restores NOTHING.

use core::sync::atomic::{AtomicU64, Ordering};

const NO_TASK: u64 = u64::MAX;
static ENTERING: [AtomicU64; crate::arch::platform_constants::MAX_CPUS] =
    [const { AtomicU64::new(NO_TASK) }; crate::arch::platform_constants::MAX_CPUS];

/// Record which task this CPU's current exception interrupted (`None` for an EL1 origin).
pub fn note_entering(cpu: usize, tid: Option<u64>) {
    if let Some(slot) = ENTERING.get(cpu) {
        slot.store(tid.unwrap_or(NO_TASK), Ordering::Relaxed);
    }
}

/// The exception diverged into the idle loop: nothing returns to the interrupted task on this
/// trap, so this is where its block (or exit) is visible.
pub fn note_idle_divergence(cpu: usize) {
    let Some(slot) = ENTERING.get(cpu) else {
        return;
    };
    let tid = slot.swap(NO_TASK, Ordering::Relaxed);
    if tid != NO_TASK {
        crate::kernel::context_witness::note_trap(
            cpu,
            0,
            false,
            crate::kernel::context_witness::TrapOrigin::User,
            Some(tid),
            None,
        );
    }
}

/// Count (and log the first few) EL0 entries whose kernel code would run under a non-kernel FPCR.
pub fn check_kernel_env() {
    let fpcr: u64;
    // SAFETY: reads FPCR only.
    unsafe {
        core::arch::asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack, preserves_flags));
    }
    crate::kernel::context_witness::note_kernel_env(fpcr == 0, fpcr, 0);
}

#[cfg(feature = "context1-clobber")]
#[repr(C, align(16))]
struct Junk([u8; 512]);

#[cfg(feature = "context1-clobber")]
static JUNK: Junk = {
    let mut b = [0u8; 512];
    let mut i = 0;
    while i < 512 {
        b[i] = 0xC5 ^ (i as u8).wrapping_mul(7);
        i += 1;
    }
    Junk(b)
};

/// Overwrite every user-visible FP/SIMD register and the user-writable control/status state. See
/// the module doc: interference, never a save or a restore.
#[cfg(feature = "context1-clobber")]
pub fn clobber_user_visible_state() {
    // FPCR: round toward zero, flush-to-zero, default NaN. FPSR: every cumulative flag + QC.
    // NZCV: all four. TPIDR_EL0: a value no task is ever given.
    let fpcr: u64 = (0b11 << 22) | (1 << 24) | (1 << 25);
    let fpsr: u64 = 0x0800_009F;
    let nzcv: u64 = 0xF000_0000;
    let tpidr: u64 = 0xC1C1_DEAD_0000_0000;
    // SAFETY: loads from a static 512-byte, 16-byte aligned image and writes system registers
    // EL0 may write itself; every vector register is declared clobbered. The kernel performs no
    // FP arithmetic and never reads TPIDR_EL0, so the junk cannot change a kernel result.
    unsafe {
        core::arch::asm!(
            "ldp q0, q1, [{j}, #0]",
            "ldp q2, q3, [{j}, #32]",
            "ldp q4, q5, [{j}, #64]",
            "ldp q6, q7, [{j}, #96]",
            "ldp q8, q9, [{j}, #128]",
            "ldp q10, q11, [{j}, #160]",
            "ldp q12, q13, [{j}, #192]",
            "ldp q14, q15, [{j}, #224]",
            "ldp q16, q17, [{j}, #256]",
            "ldp q18, q19, [{j}, #288]",
            "ldp q20, q21, [{j}, #320]",
            "ldp q22, q23, [{j}, #352]",
            "ldp q24, q25, [{j}, #384]",
            "ldp q26, q27, [{j}, #416]",
            "ldp q28, q29, [{j}, #448]",
            "ldp q30, q31, [{j}, #480]",
            "msr fpcr, {c}",
            "msr fpsr, {s}",
            "msr nzcv, {n}",
            "msr tpidr_el0, {t}",
            j = in(reg) &JUNK,
            c = in(reg) fpcr,
            s = in(reg) fpsr,
            n = in(reg) nzcv,
            t = in(reg) tpidr,
            out("v0") _, out("v1") _, out("v2") _, out("v3") _,
            out("v4") _, out("v5") _, out("v6") _, out("v7") _,
            out("v8") _, out("v9") _, out("v10") _, out("v11") _,
            out("v12") _, out("v13") _, out("v14") _, out("v15") _,
            out("v16") _, out("v17") _, out("v18") _, out("v19") _,
            out("v20") _, out("v21") _, out("v22") _, out("v23") _,
            out("v24") _, out("v25") _, out("v26") _, out("v27") _,
            out("v28") _, out("v29") _, out("v30") _, out("v31") _,
            options(nostack, readonly),
        );
    }
}
