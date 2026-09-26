// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-CONTEXT1 §3 — init's user execution-state witness (slot-5 selector 31).
//!
//! Compiled only with `context1-witness` on x86_64 or AArch64. It uses nothing but existing
//! task creation (`spawn_thread`, NR 11), scheduling (the timer), a deadline receive (NR 5) on a
//! private kernel-provisioned park endpoint (slot 14), and `ExitCurrentTask` (NR 16).
//!
//! Every assertion is made on a FULL architectural image, loaded and captured inside ONE asm
//! block so no compiler spill can stand in for the kernel's preservation:
//!
//! * x86_64: the whole FXSAVE image — FCW, FSW, FTW, MXCSR, ST0..ST7 (80 bits) and XMM0..XMM15
//!   (128 bits) — plus RFLAGS' arithmetic flags and DF;
//! * AArch64: q0..q31 (128 bits), FPCR, FPSR, NZCV and TPIDR_EL0.
//!
//! Four cells, in this order:
//!
//! 1. **fresh** — thread C is created while A holds a junk FP/SIMD state and blocks; C's very
//!    first instructions capture its state, which must be the architectural initial state.
//! 2. **block** — A loads its pattern, blocks on a deadline receive with nothing else runnable
//!    (a genuine idle interval), is resumed, and captures. Asserted: FP/SIMD, control/status,
//!    TPIDR_EL0. NOT asserted: flags and GPRs the syscall ABI clobbers.
//! 3. **preempt** — thread B runs its own distinct pattern and control settings in a loop and
//!    clears A's `ran_not` word. A loads its pattern and spins, flag-neutrally, until it observes
//!    `ran_not == 0`: on one CPU that is only possible if A was descheduled and B executed between
//!    A's capture and A's restoration. Asserted: everything, including flags, and the six GPRs
//!    that are the syscall argument/result lanes (x0..x5; rdi rsi rdx r10 r8 r9), which A holds
//!    live sentinels in across the window. The round's mode chooses how A comes back: `spin`
//!    rounds keep B spinning, so only a timer tick can return to A; `block` rounds make B block
//!    right after its first short window, so A resumes through B's blocking SYSCALL return.
//! 4. **same** — B has exited; A spins for several quanta (sized from cell 3) under timer
//!    interrupts that return to A. Asserted: everything, including flags; one round with DF set.
//!
//! Markers keep one prefix, `CTX1_`, so the grader can relate them to the kernel's `CTX1_TRAP`
//! identity lines.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering::SeqCst};

pub(super) const SELECTOR: u32 = 31;

pub(super) fn armed(slot5: Option<u32>) -> bool {
    matches!(slot5, Some(SELECTOR))
}

const NR_IPC_RECV_TIMEOUT: usize = 5;
const ROUNDS: u32 = 3;
const BLOCK_TICKS: u64 = 6;
const FRESH_PARK_TICKS: u64 = 4;
/// Upper bound on one preemption window's wait for B, in loop iterations.
const PREEMPT_BOUND: u64 = 4_000_000_000;
const B_ITERS: u64 = 3_000_000;
/// B's window in a `block` round: short, so B blocks before a tick can intervene.
const B_SHORT_ITERS: u64 = 4_096;
/// Per-round B behaviour: 0 = spin (A returns on a timer tick), 1 = block after one short window
/// (A returns through B's blocking syscall).
static B_MODE: AtomicU32 = AtomicU32::new(0);
static B_PARK: AtomicU32 = AtomicU32::new(0);
/// A's six live GPR sentinels across a preemption window.
const GPR_SENTINELS: [usize; 6] = [
    0x5A5A_0000_0000_0001,
    0x5A5A_0000_0000_0102,
    0x5A5A_0000_0000_0203,
    0x5A5A_0000_0000_0304,
    0x5A5A_0000_0000_0405,
    0x5A5A_0000_0000_0506,
];
const STACK_BYTES: usize = 16 * 1024;

#[repr(C, align(16))]
struct Stack([u8; STACK_BYTES]);
static mut B_STACK: Stack = Stack([0; STACK_BYTES]);
static mut C_STACK: Stack = Stack([0; STACK_BYTES]);
static mut B_TLS: [u8; 256] = [0; 256];
static mut C_TLS: [u8; 256] = [0; 256];

/// A's "B has not run yet" word: A sets it to 1, B stores 0 on every loop iteration.
static RAN_NOT: AtomicU64 = AtomicU64::new(1);
static B_STOP: AtomicU32 = AtomicU32::new(0);
static B_DONE: AtomicU32 = AtomicU32::new(0);
static B_WINDOWS: AtomicU32 = AtomicU32::new(0);
static B_BAD: AtomicU32 = AtomicU32::new(0);
static B_FIRST_BAD: AtomicU64 = AtomicU64::new(0);
static C_DONE: AtomicU32 = AtomicU32::new(0);

// ─────────────────────────────── x86_64 ───────────────────────────────

#[cfg(target_arch = "x86_64")]
mod arch {
    use super::*;

    /// One FXSAVE64 image.
    #[repr(C, align(16))]
    #[derive(Clone, Copy)]
    pub struct State(pub [u8; 512]);

    pub const ZERO: State = State([0; 512]);

    /// RFLAGS bits the witness asserts: CF PF AF ZF SF OF, and DF.
    pub const FLAG_MASK: u64 = 0x0CD5;
    pub const RFLAGS_BIT1: u64 = 0x2;

    pub fn pattern(seed: u8, fcw: u16, mxcsr: u32) -> State {
        let mut b = [0u8; 512];
        b[0..2].copy_from_slice(&fcw.to_le_bytes());
        // FTW (abridged): R0 valid; FSW.TOP = 0, so that is ST0.
        b[4] = 0x01;
        b[24..28].copy_from_slice(&mxcsr.to_le_bytes());
        for k in 0..10 {
            b[32 + k] = seed ^ (0x31u8.wrapping_mul(k as u8 + 1));
        }
        for i in 0..16 {
            for k in 0..16 {
                b[160 + 16 * i + k] =
                    seed.wrapping_add((i * 16 + k) as u8).wrapping_mul(0x9D) ^ 0x5A;
            }
        }
        State(b)
    }

    /// The architectural initial state the kernel promises a fresh task (`fninit` + MXCSR
    /// default): FCW 0x037F, FSW 0, FTW empty, MXCSR 0x1F80, every data register zero.
    pub fn initial() -> State {
        let mut b = [0u8; 512];
        b[0..2].copy_from_slice(&0x037Fu16.to_le_bytes());
        b[24..28].copy_from_slice(&0x1F80u32.to_le_bytes());
        State(b)
    }

    /// Field mismatch mask: bit0 FCW, bit1 FSW, bit2 FTW, bit3 MXCSR, bits 4..11 ST0..ST7,
    /// bits 12..27 XMM0..XMM15. MXCSR_MASK, FOP/FIP/FDP and the reserved tail are not user state.
    pub fn diff(want: &State, got: &State) -> u64 {
        let (w, g) = (&want.0, &got.0);
        let mut m = 0u64;
        if w[0..2] != g[0..2] {
            m |= 1;
        }
        if w[2..4] != g[2..4] {
            m |= 1 << 1;
        }
        if w[4] != g[4] {
            m |= 1 << 2;
        }
        if w[24..28] != g[24..28] {
            m |= 1 << 3;
        }
        for i in 0..8 {
            if w[32 + 16 * i..42 + 16 * i] != g[32 + 16 * i..42 + 16 * i] {
                m |= 1 << (4 + i);
            }
        }
        for i in 0..16 {
            if w[160 + 16 * i..176 + 16 * i] != g[160 + 16 * i..176 + 16 * i] {
                m |= 1 << (12 + i);
            }
        }
        m
    }

    /// Load `pat` and `flags`, spin until B has run (`RAN_NOT == 0`) or `bound` iterations pass,
    /// capture into `out`. Returns `(flags_out, iterations_left)`. Flag-neutral throughout:
    /// `mov`, `lea`, `jrcxz`, `jmp` only.
    pub unsafe fn spin_until_other_ran(
        pat: &State,
        out: &mut State,
        flags: u64,
        bound: u64,
        gprs: &mut [usize; 6],
    ) -> (u64, u64) {
        let mut env = ZERO;
        let flags_out: u64;
        let left: u64;
        unsafe {
            core::arch::asm!(
                "fxsave64 [{env}]",
                "fxrstor64 [{pat}]",
                "push {fin}",
                "popfq",
                "2:",
                "mov rcx, qword ptr [{ran}]",
                "jrcxz 3f",
                "mov rcx, {n}",
                "jrcxz 3f",
                "lea {n}, [{n} - 1]",
                "jmp 2b",
                "3:",
                "pushfq",
                "pop {fout}",
                // The Rust ABI requires DF clear at every call boundary; the pattern may set it.
                "cld",
                "fxsave64 [{out}]",
                "fxrstor64 [{env}]",
                env = in(reg) &mut env,
                pat = in(reg) pat,
                out = in(reg) out,
                fin = in(reg) flags | RFLAGS_BIT1,
                ran = in(reg) RAN_NOT.as_ptr(),
                n = inout(reg) bound => left,
                fout = out(reg) flags_out,
                inout("rdi") gprs[0],
                inout("rsi") gprs[1],
                inout("rdx") gprs[2],
                inout("r10") gprs[3],
                inout("r8") gprs[4],
                inout("r9") gprs[5],
                out("rcx") _,
                out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
                out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
                out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
                out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
            );
        }
        (flags_out, left)
    }

    /// Load `pat` and `flags`, spin exactly `iters` iterations (`loop` touches no flag), storing 0
    /// to `RAN_NOT` each time when `clear_ran` is set, capture into `out`. Returns `flags_out`.
    pub unsafe fn spin_fixed(
        pat: &State,
        out: &mut State,
        flags: u64,
        iters: u64,
        clear_ran: bool,
    ) -> u64 {
        let mut env = ZERO;
        let flags_out: u64;
        let target: *mut u64 = if clear_ran {
            RAN_NOT.as_ptr()
        } else {
            core::ptr::null_mut()
        };
        let mut scratch: u64 = 0;
        let store_to = if target.is_null() {
            &mut scratch as *mut u64
        } else {
            target
        };
        unsafe {
            core::arch::asm!(
                "fxsave64 [{env}]",
                "fxrstor64 [{pat}]",
                "push {fin}",
                "popfq",
                "2:",
                "mov qword ptr [{st}], 0",
                "loop 2b",
                "pushfq",
                "pop {fout}",
                // The Rust ABI requires DF clear at every call boundary; the pattern may set it.
                "cld",
                "fxsave64 [{out}]",
                "fxrstor64 [{env}]",
                env = in(reg) &mut env,
                pat = in(reg) pat,
                out = in(reg) out,
                fin = in(reg) flags | RFLAGS_BIT1,
                st = in(reg) store_to,
                fout = out(reg) flags_out,
                inout("rcx") iters.max(1) => _,
                out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
                out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
                out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
                out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
            );
        }
        flags_out
    }

    /// Load `pat`, block on a deadline receive, capture into `out` right after the syscall.
    /// Returns the syscall error lane (9 = timed out).
    pub unsafe fn block_with(pat: &State, out: &mut State, park: u32, ticks: u64) -> usize {
        let mut env = ZERO;
        let mut payload = [0u8; 64];
        let mut meta = [0u64; 5];
        meta[0] = u64::MAX;
        let err: usize;
        unsafe {
            core::arch::asm!(
                "fxsave64 [{env}]",
                "fxrstor64 [{pat}]",
                "syscall",
                "fxsave64 [{out}]",
                "fxrstor64 [{env}]",
                env = in(reg) &mut env,
                pat = in(reg) pat,
                out = in(reg) out,
                inlateout("rax") NR_IPC_RECV_TIMEOUT => _,
                in("rdi") park as usize,
                in("rsi") payload.as_mut_ptr() as usize,
                inlateout("rdx") payload.len() => _,
                in("r10") ticks as usize,
                inlateout("r8") meta.as_mut_ptr() as usize => _,
                in("r9") 40usize,
                lateout("rcx") err,
                lateout("r11") _,
                out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
                out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
                out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
                out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
            );
        }
        err
    }

    #[unsafe(no_mangle)]
    pub static mut CTX1_FRESH_IMAGE: State = ZERO;
    #[unsafe(no_mangle)]
    pub static mut CTX1_FRESH_FLAGS: u64 = 0;

    // Thread C's entry: capture the whole FXSAVE image and RFLAGS before ANY other instruction
    // can touch them, then continue in Rust on a 16-byte aligned stack.
    core::arch::global_asm!(
        ".global yarm_ctx1_fresh_entry",
        "yarm_ctx1_fresh_entry:",
        "fxsave64 [rip + CTX1_FRESH_IMAGE]",
        "pushfq",
        "pop qword ptr [rip + CTX1_FRESH_FLAGS]",
        "and rsp, -16",
        "call {body}",
        "ud2",
        body = sym super::fresh_body,
    );
    // Thread B's entry: SysV alignment, then Rust.
    core::arch::global_asm!(
        ".global yarm_ctx1_b_entry",
        "yarm_ctx1_b_entry:",
        "and rsp, -16",
        "call {body}",
        "ud2",
        body = sym super::b_body,
    );
    unsafe extern "C" {
        pub fn yarm_ctx1_fresh_entry();
        pub fn yarm_ctx1_b_entry();
    }

    pub fn fresh_capture() -> (State, u64) {
        // SAFETY: written once by C's entry before C_DONE is published.
        unsafe {
            (
                core::ptr::read_volatile(core::ptr::addr_of!(CTX1_FRESH_IMAGE)),
                core::ptr::read_volatile(core::ptr::addr_of!(CTX1_FRESH_FLAGS)),
            )
        }
    }

    pub const A_FLAGS: u64 = 0x0001 | 0x0004 | 0x0040 | 0x0800; // CF PF ZF OF
    pub const A_FLAGS_DF: u64 = A_FLAGS | 0x0400;
    pub const B_FLAGS: u64 = 0x0001 | 0x0010 | 0x0080; // CF AF SF
    pub fn a_pattern() -> State {
        pattern(0xA1, 0x0F7F, 0x3F80) // FCW RC=chop; MXCSR RC=down
    }
    pub fn a_block_pattern() -> State {
        pattern(0xA7, 0x0B7F, 0x3F80)
    }
    pub fn b_pattern() -> State {
        pattern(0xB2, 0x027F, 0xDF80) // FCW PC=53-bit; MXCSR RC=up|FZ
    }
    pub fn junk_pattern() -> State {
        pattern(0xEE, 0x0C7F, 0x7F80)
    }
    pub fn flags_diff(want: u64, got: u64) -> bool {
        want & FLAG_MASK != got & FLAG_MASK
    }
}

// ─────────────────────────────── AArch64 ───────────────────────────────

#[cfg(target_arch = "aarch64")]
mod arch {
    use super::*;

    /// q0..q31, then FPCR, FPSR, NZCV, TPIDR_EL0.
    #[repr(C, align(16))]
    #[derive(Clone, Copy)]
    pub struct State(pub [u8; 544]);

    pub const ZERO: State = State([0; 544]);
    pub const FLAG_MASK: u64 = 0xF000_0000;

    pub fn pattern(seed: u8, fpcr: u64, fpsr: u64, nzcv: u64, tpidr: u64) -> State {
        let mut b = [0u8; 544];
        for i in 0..32 {
            for k in 0..16 {
                b[16 * i + k] = seed.wrapping_add((i * 16 + k) as u8).wrapping_mul(0x9D) ^ 0x5A;
            }
        }
        b[512..520].copy_from_slice(&fpcr.to_le_bytes());
        b[520..528].copy_from_slice(&fpsr.to_le_bytes());
        b[528..536].copy_from_slice(&nzcv.to_le_bytes());
        b[536..544].copy_from_slice(&tpidr.to_le_bytes());
        State(b)
    }

    pub fn initial(tls: u64) -> State {
        pattern_zero_with_tpidr(tls)
    }

    fn pattern_zero_with_tpidr(tpidr: u64) -> State {
        let mut b = [0u8; 544];
        b[536..544].copy_from_slice(&tpidr.to_le_bytes());
        State(b)
    }

    /// bits 0..31 q0..q31, bit 32 FPCR, bit 33 FPSR, bit 34 NZCV, bit 35 TPIDR_EL0.
    pub fn diff_with(want: &State, got: &State, check_nzcv: bool) -> u64 {
        let (w, g) = (&want.0, &got.0);
        let mut m = 0u64;
        for i in 0..32 {
            if w[16 * i..16 * i + 16] != g[16 * i..16 * i + 16] {
                m |= 1 << i;
            }
        }
        if w[512..520] != g[512..520] {
            m |= 1 << 32;
        }
        if w[520..528] != g[520..528] {
            m |= 1 << 33;
        }
        if check_nzcv && w[528..536] != g[528..536] {
            m |= 1 << 34;
        }
        if w[536..544] != g[536..544] {
            m |= 1 << 35;
        }
        m
    }

    pub fn diff(want: &State, got: &State) -> u64 {
        diff_with(want, got, false)
    }

    macro_rules! load_state {
        () => {
            concat!(
                "ldp q0, q1, [{pat}, #0]\n",
                "ldp q2, q3, [{pat}, #32]\n",
                "ldp q4, q5, [{pat}, #64]\n",
                "ldp q6, q7, [{pat}, #96]\n",
                "ldp q8, q9, [{pat}, #128]\n",
                "ldp q10, q11, [{pat}, #160]\n",
                "ldp q12, q13, [{pat}, #192]\n",
                "ldp q14, q15, [{pat}, #224]\n",
                "ldp q16, q17, [{pat}, #256]\n",
                "ldp q18, q19, [{pat}, #288]\n",
                "ldp q20, q21, [{pat}, #320]\n",
                "ldp q22, q23, [{pat}, #352]\n",
                "ldp q24, q25, [{pat}, #384]\n",
                "ldp q26, q27, [{pat}, #416]\n",
                "ldp q28, q29, [{pat}, #448]\n",
                "ldp q30, q31, [{pat}, #480]\n",
                "ldr {t}, [{pat}, #512]\n",
                "msr fpcr, {t}\n",
                "ldr {t}, [{pat}, #520]\n",
                "msr fpsr, {t}\n",
                "ldr {t}, [{pat}, #536]\n",
                "msr tpidr_el0, {t}\n",
                "ldr {t}, [{pat}, #528]\n",
                "msr nzcv, {t}\n",
            )
        };
    }
    macro_rules! store_state {
        () => {
            concat!(
                "mrs {t}, nzcv\n",
                "str {t}, [{out}, #528]\n",
                "mrs {t}, fpcr\n",
                "str {t}, [{out}, #512]\n",
                "mrs {t}, fpsr\n",
                "str {t}, [{out}, #520]\n",
                "mrs {t}, tpidr_el0\n",
                "str {t}, [{out}, #536]\n",
                "stp q0, q1, [{out}, #0]\n",
                "stp q2, q3, [{out}, #32]\n",
                "stp q4, q5, [{out}, #64]\n",
                "stp q6, q7, [{out}, #96]\n",
                "stp q8, q9, [{out}, #128]\n",
                "stp q10, q11, [{out}, #160]\n",
                "stp q12, q13, [{out}, #192]\n",
                "stp q14, q15, [{out}, #224]\n",
                "stp q16, q17, [{out}, #256]\n",
                "stp q18, q19, [{out}, #288]\n",
                "stp q20, q21, [{out}, #320]\n",
                "stp q22, q23, [{out}, #352]\n",
                "stp q24, q25, [{out}, #384]\n",
                "stp q26, q27, [{out}, #416]\n",
                "stp q28, q29, [{out}, #448]\n",
                "stp q30, q31, [{out}, #480]\n",
            )
        };
    }
    macro_rules! save_env {
        () => {
            "mrs {e0}, fpcr\nmrs {e1}, fpsr\nmrs {e2}, tpidr_el0\n"
        };
    }
    macro_rules! restore_env {
        () => {
            "msr fpcr, {e0}\nmsr fpsr, {e1}\nmsr tpidr_el0, {e2}\n"
        };
    }

    /// Spin until B has run (`RAN_NOT == 0`) or `bound` iterations pass; NZCV-neutral (`ldr`,
    /// `cbz`, `sub`, `b` only). Returns iterations left.
    pub unsafe fn spin_until_other_ran(
        pat: &State,
        out: &mut State,
        bound: u64,
        gprs: &mut [usize; 6],
    ) -> u64 {
        let left: u64;
        unsafe {
            core::arch::asm!(
                save_env!(),
                load_state!(),
                "2:",
                "ldr {t}, [{ran}]",
                "cbz {t}, 3f",
                "cbz {n}, 3f",
                "sub {n}, {n}, #1",
                "b 2b",
                "3:",
                store_state!(),
                restore_env!(),
                pat = in(reg) pat,
                out = in(reg) out,
                ran = in(reg) RAN_NOT.as_ptr(),
                n = inout(reg) bound => left,
                t = out(reg) _,
                e0 = out(reg) _, e1 = out(reg) _, e2 = out(reg) _,
                inout("x0") gprs[0],
                inout("x1") gprs[1],
                inout("x2") gprs[2],
                inout("x3") gprs[3],
                inout("x4") gprs[4],
                inout("x5") gprs[5],
                out("v0") _, out("v1") _, out("v2") _, out("v3") _,
                out("v4") _, out("v5") _, out("v6") _, out("v7") _,
                out("v8") _, out("v9") _, out("v10") _, out("v11") _,
                out("v12") _, out("v13") _, out("v14") _, out("v15") _,
                out("v16") _, out("v17") _, out("v18") _, out("v19") _,
                out("v20") _, out("v21") _, out("v22") _, out("v23") _,
                out("v24") _, out("v25") _, out("v26") _, out("v27") _,
                out("v28") _, out("v29") _, out("v30") _, out("v31") _,
                options(nostack),
            );
        }
        left
    }

    /// Spin exactly `iters` iterations (NZCV-neutral), storing 0 to `RAN_NOT` each time when
    /// `clear_ran` is set.
    pub unsafe fn spin_fixed(pat: &State, out: &mut State, iters: u64, clear_ran: bool) {
        let mut scratch: u64 = 0;
        let st: *mut u64 = if clear_ran {
            RAN_NOT.as_ptr()
        } else {
            &mut scratch
        };
        unsafe {
            core::arch::asm!(
                save_env!(),
                load_state!(),
                "2:",
                "str xzr, [{st}]",
                "sub {n}, {n}, #1",
                "cbnz {n}, 2b",
                store_state!(),
                restore_env!(),
                pat = in(reg) pat,
                out = in(reg) out,
                st = in(reg) st,
                n = inout(reg) iters.max(1) => _,
                t = out(reg) _,
                e0 = out(reg) _, e1 = out(reg) _, e2 = out(reg) _,
                out("v0") _, out("v1") _, out("v2") _, out("v3") _,
                out("v4") _, out("v5") _, out("v6") _, out("v7") _,
                out("v8") _, out("v9") _, out("v10") _, out("v11") _,
                out("v12") _, out("v13") _, out("v14") _, out("v15") _,
                out("v16") _, out("v17") _, out("v18") _, out("v19") _,
                out("v20") _, out("v21") _, out("v22") _, out("v23") _,
                out("v24") _, out("v25") _, out("v26") _, out("v27") _,
                out("v28") _, out("v29") _, out("v30") _, out("v31") _,
                options(nostack),
            );
        }
    }

    /// Load `pat`, block on a deadline receive (`svc`), capture right after. Returns `x0`.
    pub unsafe fn block_with(pat: &State, out: &mut State, park: u32, ticks: u64) -> usize {
        let mut payload = [0u8; 64];
        let mut meta = [0u64; 5];
        meta[0] = u64::MAX;
        let x0: usize;
        unsafe {
            core::arch::asm!(
                save_env!(),
                load_state!(),
                "svc #0",
                store_state!(),
                restore_env!(),
                pat = in(reg) pat,
                out = in(reg) out,
                t = out(reg) _,
                e0 = out(reg) _, e1 = out(reg) _, e2 = out(reg) _,
                inlateout("x0") park as usize => x0,
                inlateout("x1") payload.as_mut_ptr() as usize => _,
                inlateout("x2") payload.len() => _,
                inlateout("x3") ticks as usize => _,
                inlateout("x4") meta.as_mut_ptr() as usize => _,
                inlateout("x5") 40usize => _,
                in("x8") NR_IPC_RECV_TIMEOUT,
                out("v0") _, out("v1") _, out("v2") _, out("v3") _,
                out("v4") _, out("v5") _, out("v6") _, out("v7") _,
                out("v8") _, out("v9") _, out("v10") _, out("v11") _,
                out("v12") _, out("v13") _, out("v14") _, out("v15") _,
                out("v16") _, out("v17") _, out("v18") _, out("v19") _,
                out("v20") _, out("v21") _, out("v22") _, out("v23") _,
                out("v24") _, out("v25") _, out("v26") _, out("v27") _,
                out("v28") _, out("v29") _, out("v30") _, out("v31") _,
                options(nostack),
            );
        }
        x0
    }

    #[unsafe(no_mangle)]
    pub static mut CTX1_FRESH_IMAGE: State = ZERO;

    // Thread C's entry: capture q0..q31, FPCR, FPSR, NZCV and TPIDR_EL0 before any other
    // instruction can touch them, then continue in Rust.
    core::arch::global_asm!(
        ".global yarm_ctx1_fresh_entry",
        "yarm_ctx1_fresh_entry:",
        "adrp x9, CTX1_FRESH_IMAGE",
        "add x9, x9, :lo12:CTX1_FRESH_IMAGE",
        "mrs x10, nzcv",
        "str x10, [x9, #528]",
        "mrs x10, fpcr",
        "str x10, [x9, #512]",
        "mrs x10, fpsr",
        "str x10, [x9, #520]",
        "mrs x10, tpidr_el0",
        "str x10, [x9, #536]",
        "stp q0, q1, [x9, #0]",
        "stp q2, q3, [x9, #32]",
        "stp q4, q5, [x9, #64]",
        "stp q6, q7, [x9, #96]",
        "stp q8, q9, [x9, #128]",
        "stp q10, q11, [x9, #160]",
        "stp q12, q13, [x9, #192]",
        "stp q14, q15, [x9, #224]",
        "stp q16, q17, [x9, #256]",
        "stp q18, q19, [x9, #288]",
        "stp q20, q21, [x9, #320]",
        "stp q22, q23, [x9, #352]",
        "stp q24, q25, [x9, #384]",
        "stp q26, q27, [x9, #416]",
        "stp q28, q29, [x9, #448]",
        "stp q30, q31, [x9, #480]",
        "b {body}",
        body = sym super::fresh_body,
    );
    core::arch::global_asm!(
        ".global yarm_ctx1_b_entry",
        "yarm_ctx1_b_entry:",
        "b {body}",
        body = sym super::b_body,
    );
    unsafe extern "C" {
        pub fn yarm_ctx1_fresh_entry();
        pub fn yarm_ctx1_b_entry();
    }

    pub fn fresh_capture() -> (State, u64) {
        // SAFETY: written once by C's entry before C_DONE is published.
        let s = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(CTX1_FRESH_IMAGE)) };
        let nzcv = u64::from_le_bytes(s.0[528..536].try_into().unwrap_or([0; 8]));
        (s, nzcv)
    }

    pub const C_TLS_VALUE: u64 = 0x0000_7E57_0000_1000;
    pub fn a_pattern() -> State {
        pattern(
            0xA1,
            1 << 22,
            0x0800_0011,
            0xA000_0000,
            0x0A0A_0000_0000_1111,
        )
    }
    pub fn a_pattern_alt() -> State {
        pattern(
            0xA3,
            3 << 22,
            0x0000_0013,
            0x6000_0000,
            0x0A0A_0000_0000_3333,
        )
    }
    pub fn a_block_pattern() -> State {
        pattern(
            0xA7,
            2 << 22,
            0x0000_0001,
            0x9000_0000,
            0x0A0A_0000_0000_7777,
        )
    }
    pub fn b_pattern() -> State {
        pattern(
            0xB2,
            (2 << 22) | (1 << 24),
            0x0000_008A,
            0x5000_0000,
            0x0B0B_0000_0000_2222,
        )
    }
    pub fn junk_pattern() -> State {
        pattern(
            0xEE,
            (3 << 22) | (1 << 25),
            0x0800_009F,
            0xF000_0000,
            0x0E0E_0000_0000_EEEE,
        )
    }
}

use arch::State;

fn recv_park(park: u32, ticks: u64) {
    // SAFETY: an ordinary deadline receive on init's private park endpoint.
    let _ = unsafe { yarm_user_rt::syscall::ipc_recv_with_deadline(park, ticks) };
}

fn exit_now() -> ! {
    loop {
        // SAFETY: ends this thread; a refusal (WouldBlock) is retried.
        let _ = unsafe { yarm_user_rt::syscall::exit_current_task() };
        let _ = yarm_user_rt::syscall::yield_now();
    }
}

extern "C" fn fresh_body() -> ! {
    C_DONE.store(1, SeqCst);
    exit_now()
}

extern "C" fn b_body() -> ! {
    let pat = arch::b_pattern();
    let mut out = arch::ZERO;
    while B_STOP.load(SeqCst) == 0 {
        let block = B_MODE.load(SeqCst) == 1;
        let iters = if block { B_SHORT_ITERS } else { B_ITERS };
        #[cfg(target_arch = "x86_64")]
        let (mask, flags_bad) = {
            // SAFETY: a self-contained register window; see `arch::spin_fixed`.
            let f = unsafe { arch::spin_fixed(&pat, &mut out, arch::B_FLAGS, iters, true) };
            (arch::diff(&pat, &out), arch::flags_diff(arch::B_FLAGS, f))
        };
        #[cfg(target_arch = "aarch64")]
        let (mask, flags_bad) = {
            // SAFETY: a self-contained register window; see `arch::spin_fixed`.
            unsafe { arch::spin_fixed(&pat, &mut out, iters, true) };
            (arch::diff_with(&pat, &out, true), false)
        };
        B_WINDOWS.fetch_add(1, SeqCst);
        if mask != 0 || flags_bad {
            if B_BAD.fetch_add(1, SeqCst) == 0 {
                B_FIRST_BAD.store(mask | ((flags_bad as u64) << 63), SeqCst);
            }
        }
        if block {
            // Hand the CPU back through a blocking syscall: A resumes on B's syscall return.
            recv_park(B_PARK.load(SeqCst), 1);
        }
    }
    B_DONE.store(1, SeqCst);
    exit_now()
}

fn spawn(entry: unsafe extern "C" fn(), stack: *mut Stack, tls: usize) -> u64 {
    let top = (stack as usize + STACK_BYTES) & !0xF;
    // SAFETY: a valid entry; the static stack outlives the thread.
    unsafe { yarm_user_rt::syscall::spawn_thread(tls, top, entry as *const () as usize) }
        .unwrap_or(0)
}

pub(super) fn run_once() {
    let park = yarm_user_rt::runtime::startup_arg_slot(
        yarm_user_rt::runtime::STARTUP_SLOT_SERVICE_EXTRA_CAP_1,
    )
    .unwrap_or(0) as u32;
    let a_tid = yarm_user_rt::runtime::startup_arg_slot(0).unwrap_or(0);
    #[cfg(target_arch = "x86_64")]
    let arch_name = "x86_64";
    #[cfg(target_arch = "aarch64")]
    let arch_name = "aarch64";
    if park == 0 {
        yarm_user_rt::user_log!(
            "CTX1_WITNESS arch={} result=fail reason=no_park_cap",
            arch_name
        );
        return;
    }
    yarm_user_rt::user_log!(
        "CTX1_WITNESS_BEGIN arch={} a_tid={} park_cap={}",
        arch_name,
        a_tid,
        park
    );
    let mut failures = 0u32;

    // ── 1. fresh ──────────────────────────────────────────────────────────────────────────────
    #[cfg(target_arch = "x86_64")]
    let c_tls = core::ptr::addr_of_mut!(C_TLS) as usize;
    #[cfg(target_arch = "aarch64")]
    let c_tls = arch::C_TLS_VALUE as usize;
    let c_tid = spawn(
        arch::yarm_ctx1_fresh_entry,
        core::ptr::addr_of_mut!(C_STACK),
        c_tls,
    );
    yarm_user_rt::user_log!(
        "CTX1_WINDOW cell=fresh round=0 a_tid={} c_tid={} begin",
        a_tid,
        c_tid
    );
    {
        let junk = arch::junk_pattern();
        let mut sink = arch::ZERO;
        // SAFETY: A carries a junk state INTO the block that lets C run, so C's first
        // instructions see whatever the kernel leaves them.
        let _ = unsafe { arch::block_with(&junk, &mut sink, park, FRESH_PARK_TICKS) };
    }
    let mut waited = 0;
    while C_DONE.load(SeqCst) == 0 && waited < 200 {
        recv_park(park, 1);
        waited += 1;
    }
    let (img, flags) = arch::fresh_capture();
    #[cfg(target_arch = "x86_64")]
    let (mask, flags_bad) = (
        arch::diff(&arch::initial(), &img),
        flags & arch::FLAG_MASK != 0,
    );
    #[cfg(target_arch = "aarch64")]
    let (mask, flags_bad) = (
        arch::diff_with(&arch::initial(c_tls as u64), &img, true),
        flags != 0,
    );
    let ok = C_DONE.load(SeqCst) == 1 && c_tid != 0 && mask == 0 && !flags_bad;
    failures += u32::from(!ok);
    yarm_user_rt::user_log!(
        "CTX1_RESULT cell=fresh round=0 tid={} ran={} mask=0x{:x} flags=0x{:x} flags_bad={} result={}",
        c_tid,
        C_DONE.load(SeqCst),
        mask,
        flags,
        flags_bad as u8,
        if ok { "ok" } else { "fail" }
    );

    // ── 2. block ──────────────────────────────────────────────────────────────────────────────
    for round in 0..ROUNDS {
        let pat = arch::a_block_pattern();
        let mut out = arch::ZERO;
        yarm_user_rt::user_log!("CTX1_WINDOW cell=block round={} tid={} begin", round, a_tid);
        // SAFETY: a self-contained register window around one blocking receive.
        let rc = unsafe { arch::block_with(&pat, &mut out, park, BLOCK_TICKS) };
        let mask = arch::diff(&pat, &out);
        let ok = mask == 0;
        failures += u32::from(!ok);
        yarm_user_rt::user_log!(
            "CTX1_RESULT cell=block round={} tid={} rc={} mask=0x{:x} result={}",
            round,
            a_tid,
            rc,
            mask,
            if ok { "ok" } else { "fail" }
        );
    }

    // ── 3. preempt ────────────────────────────────────────────────────────────────────────────
    B_STOP.store(0, SeqCst);
    B_DONE.store(0, SeqCst);
    B_PARK.store(park, SeqCst);
    B_MODE.store(1, SeqCst);
    let b_tid = spawn(
        arch::yarm_ctx1_b_entry,
        core::ptr::addr_of_mut!(B_STACK),
        core::ptr::addr_of_mut!(B_TLS) as usize,
    );
    let mut quantum_iters = 0u64;
    for round in 0..ROUNDS {
        let pat = arch::a_pattern();
        let mut out = arch::ZERO;
        let mut gprs = GPR_SENTINELS;
        // Rounds 0 and 2 return A through B's blocking syscall, round 1 on a timer tick.
        let mode = if round == 1 { 0 } else { 1 };
        B_MODE.store(mode, SeqCst);
        RAN_NOT.store(1, SeqCst);
        let b0 = B_WINDOWS.load(SeqCst);
        yarm_user_rt::user_log!(
            "CTX1_WINDOW cell=preempt round={} tid={} b_tid={} mode={} begin",
            round,
            a_tid,
            b_tid,
            if mode == 1 { "block" } else { "spin" }
        );
        #[cfg(target_arch = "x86_64")]
        let (left, flags_bad) = {
            // SAFETY: a self-contained register window; see `arch::spin_until_other_ran`.
            let (f, left) = unsafe {
                arch::spin_until_other_ran(&pat, &mut out, arch::A_FLAGS, PREEMPT_BOUND, &mut gprs)
            };
            (left, arch::flags_diff(arch::A_FLAGS, f))
        };
        #[cfg(target_arch = "aarch64")]
        let (left, flags_bad) = {
            // SAFETY: a self-contained register window; see `arch::spin_until_other_ran`.
            let left =
                unsafe { arch::spin_until_other_ran(&pat, &mut out, PREEMPT_BOUND, &mut gprs) };
            (left, false)
        };
        #[cfg(target_arch = "x86_64")]
        let mask = arch::diff(&pat, &out);
        #[cfg(target_arch = "aarch64")]
        let mask = arch::diff_with(&pat, &out, true);
        let b_ran = RAN_NOT.load(SeqCst) == 0;
        let used = PREEMPT_BOUND - left;
        if mode == 0 {
            quantum_iters = quantum_iters.max(used);
        }
        let mut gpr_bad = 0u32;
        for (i, (&want, &got)) in GPR_SENTINELS.iter().zip(gprs.iter()).enumerate() {
            if want != got {
                gpr_bad |= 1 << i;
            }
        }
        let ok = b_ran && mask == 0 && !flags_bad && gpr_bad == 0;
        failures += u32::from(!ok);
        yarm_user_rt::user_log!(
            "CTX1_RESULT cell=preempt round={} tid={} b_tid={} mode={} b_ran={} b_windows={} iters={} mask=0x{:x} flags_bad={} gpr_bad=0x{:x} result={}",
            round,
            a_tid,
            b_tid,
            if mode == 1 { "block" } else { "spin" },
            b_ran as u8,
            B_WINDOWS.load(SeqCst) - b0,
            used,
            mask,
            flags_bad as u8,
            gpr_bad,
            if ok { "ok" } else { "fail" }
        );
    }
    B_MODE.store(0, SeqCst);
    B_STOP.store(1, SeqCst);
    let mut waited = 0;
    while B_DONE.load(SeqCst) == 0 && waited < 400 {
        recv_park(park, 1);
        waited += 1;
    }
    let b_ok = b_tid != 0
        && B_BAD.load(SeqCst) == 0
        && B_WINDOWS.load(SeqCst) > 0
        && B_DONE.load(SeqCst) == 1;
    failures += u32::from(!b_ok);
    yarm_user_rt::user_log!(
        "CTX1_RESULT cell=preempt_b tid={} windows={} bad={} first_bad=0x{:x} done={} result={}",
        b_tid,
        B_WINDOWS.load(SeqCst),
        B_BAD.load(SeqCst),
        B_FIRST_BAD.load(SeqCst),
        B_DONE.load(SeqCst),
        if b_ok { "ok" } else { "fail" }
    );

    // ── 4. same ───────────────────────────────────────────────────────────────────────────────
    let same_iters = quantum_iters
        .saturating_mul(4)
        .clamp(1 << 20, PREEMPT_BOUND);
    for round in 0..ROUNDS {
        let mut out = arch::ZERO;
        #[cfg(target_arch = "x86_64")]
        let (pat, want_flags) = if round == ROUNDS - 1 {
            (arch::a_pattern(), arch::A_FLAGS_DF)
        } else {
            (arch::a_pattern(), arch::A_FLAGS)
        };
        #[cfg(target_arch = "aarch64")]
        let pat = if round == ROUNDS - 1 {
            arch::a_pattern_alt()
        } else {
            arch::a_pattern()
        };
        yarm_user_rt::user_log!(
            "CTX1_WINDOW cell=same round={} tid={} iters={} begin",
            round,
            a_tid,
            same_iters
        );
        #[cfg(target_arch = "x86_64")]
        let (mask, flags_bad) = {
            // SAFETY: a self-contained register window; see `arch::spin_fixed`.
            let f = unsafe { arch::spin_fixed(&pat, &mut out, want_flags, same_iters, false) };
            (arch::diff(&pat, &out), arch::flags_diff(want_flags, f))
        };
        #[cfg(target_arch = "aarch64")]
        let (mask, flags_bad) = {
            // SAFETY: a self-contained register window; see `arch::spin_fixed`.
            unsafe { arch::spin_fixed(&pat, &mut out, same_iters, false) };
            (arch::diff_with(&pat, &out, true), false)
        };
        let ok = mask == 0 && !flags_bad;
        failures += u32::from(!ok);
        yarm_user_rt::user_log!(
            "CTX1_RESULT cell=same round={} tid={} mask=0x{:x} flags_bad={} result={}",
            round,
            a_tid,
            mask,
            flags_bad as u8,
            if ok { "ok" } else { "fail" }
        );
    }

    yarm_user_rt::user_log!(
        "CTX1_WITNESS arch={} a_tid={} b_tid={} c_tid={} failures={} result={}",
        arch_name,
        a_tid,
        b_tid,
        c_tid,
        failures,
        if failures == 0 { "ok" } else { "fail" }
    );
}
