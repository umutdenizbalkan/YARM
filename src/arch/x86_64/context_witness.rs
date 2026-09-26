// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-CONTEXT1 §3 — the x86_64 half of the execution-state witness's kernel hooks.
//!
//! * [`check_kernel_env`] — entered from ring 3, is the kernel running under ITS OWN x87/SSE
//!   control environment (FCW `0x037F`, MXCSR control bits `0x1F80`, DF clear), or under the
//!   interrupted task's?
//! * [`clobber_user_visible_state`] (`context1-clobber`) — after dispatch, overwrite every
//!   user-visible x87/SSE register and control word with junk. It saves and restores NOTHING: a
//!   user state the trap path did not capture before this point, or does not restore after it,
//!   is lost here, deterministically, instead of only when some kernel code happened to touch it.

/// Read MXCSR, FCW and RFLAGS.DF as the kernel sees them right now.
fn current_env() -> (u32, u16, bool) {
    let mut mxcsr: u32 = 0;
    let mut fcw: u16 = 0;
    let rflags: u64;
    // SAFETY: stores into two locals; no state is changed.
    unsafe {
        core::arch::asm!(
            "stmxcsr [{m}]",
            "fnstcw [{c}]",
            "pushfq",
            "pop {f}",
            m = in(reg) &mut mxcsr,
            c = in(reg) &mut fcw,
            f = out(reg) rflags,
        );
    }
    (mxcsr, fcw, rflags & (1 << 10) != 0)
}

/// Count (and log the first few) entries from ring 3 whose kernel code would run under a control
/// environment that is not the kernel's own.
pub fn check_kernel_env() {
    let (mxcsr, fcw, df) = current_env();
    // MXCSR bits 0..5 are sticky exception FLAGS the kernel's own SSE may set; only the control
    // bits (masks, rounding, FZ, DAZ) are environment.
    let ok = mxcsr & 0xFFC0 == 0x1F80 && fcw & 0x1F3F == 0x033F && !df;
    crate::kernel::context_witness::note_kernel_env(
        ok,
        mxcsr as u64,
        (fcw as u64) | ((df as u64) << 16),
    );
}

/// One 512-byte FXSAVE image, as FXRSTOR64 reads it.
#[repr(C, align(16))]
struct FxImage([u8; 512]);

const fn junk_image() -> FxImage {
    let mut b = [0u8; 512];
    // FCW: round toward zero, all exceptions masked.
    b[0] = 0x7F;
    b[1] = 0x0C;
    // FTW (abridged): every x87 register valid, so the junk reaches ST0..ST7 as well.
    b[4] = 0xFF;
    // MXCSR: round toward zero, FZ, DAZ, all exceptions masked.
    b[24] = 0xC0;
    b[25] = 0xFF;
    // ST0..ST7 (10 bytes each, 16-byte slots) and XMM0..XMM15: a junk byte pattern.
    let mut i = 32;
    while i < 160 {
        if (i - 32) % 16 < 10 {
            b[i] = 0xD1 ^ (i as u8);
        }
        i += 1;
    }
    while i < 416 {
        b[i] = 0xC5 ^ (i as u8).wrapping_mul(7);
        i += 1;
    }
    FxImage(b)
}

#[cfg(feature = "context1-clobber")]
static JUNK: FxImage = junk_image();

/// Overwrite x87 ST0..7, FCW, FSW/FTW, MXCSR and XMM0..15 with junk. See the module doc: this is
/// the witness's interference, never a save or a restore.
#[cfg(feature = "context1-clobber")]
pub fn clobber_user_visible_state() {
    // SAFETY: FXRSTOR64 of a valid, 16-byte aligned image with reserved MXCSR bits clear. Every
    // register it writes is declared clobbered; the kernel performs no FP arithmetic, so the
    // junk control words cannot change the result of any kernel computation.
    unsafe {
        core::arch::asm!(
            "fxrstor64 [{0}]",
            in(reg) &JUNK,
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
            out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
            out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
            out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
            out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
            out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
            options(nostack, readonly),
        );
    }
}

#[cfg(not(feature = "context1-clobber"))]
#[allow(dead_code)]
const _: FxImage = junk_image();
